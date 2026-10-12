//! Draws 4:2:0 frames with wgpu, converting YUV to RGB in the fragment shader and
//! letterboxing to keep the aspect ratio. I420 uses three single-channel textures; NV12
//! uses a single-channel luma texture plus a two-channel (CbCr) chroma texture.

use std::sync::Arc;

use anyhow::{Context, Result};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::decode::{self, YuvFrame};

/// An egui frame to draw over (or instead of) the video.
pub struct UiFrame {
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures: egui::TexturesDelta,
    pub pixels_per_point: f32,
}

const SHADER: &str = r#"
struct Params {
    scale: vec2<f32>,
    // 1.0 when the surface is sRGB and will gamma-encode our output a second time.
    linearize: f32,
    // 1.0 when chroma is interleaved in tex_u (NV12); tex_v is then unused.
    nv12: f32,
};

@group(0) @binding(0) var tex_y: texture_2d<f32>;
@group(0) @binding(1) var tex_u: texture_2d<f32>;
@group(0) @binding(2) var tex_v: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> params: Params;

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    let p = corners[i];
    var out: VertexOut;
    out.pos = vec4(p * params.scale, 0.0, 1.0);
    out.uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // BT.709, limited ("video") range, which is what the host captures.
    let y = (textureSample(tex_y, samp, in.uv).r - 16.0 / 255.0) * (255.0 / 219.0);
    let chroma = textureSample(tex_u, samp, in.uv);
    var cr_raw = textureSample(tex_v, samp, in.uv).r;
    if params.nv12 > 0.5 {
        cr_raw = chroma.g;
    }
    let cb = (chroma.r - 128.0 / 255.0) * (255.0 / 224.0);
    let cr = (cr_raw - 128.0 / 255.0) * (255.0 / 224.0);
    var rgb = clamp(vec3(
        y + 1.5748 * cr,
        y - 0.1873 * cb - 0.4681 * cr,
        y + 1.8556 * cb,
    ), vec3(0.0), vec3(1.0));
    if params.linearize > 0.5 {
        rgb = pow(rgb, vec3(2.2));
    }
    return vec4(rgb, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    scale: [f32; 2],
    linearize: f32,
    nv12: f32,
}

struct Planes {
    width: u32,
    height: u32,
    nv12: bool,
    /// Our own textures that frames are copied into (reusable for the next frame), as
    /// opposed to a decoder's texture imported for zero copy.
    owned: bool,
    y: wgpu::Texture,
    /// U plane (I420) or interleaved CbCr (NV12).
    u: wgpu::Texture,
    /// V plane; NV12 has none.
    v: Option<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
    /// Keeps a decoder's buffer reserved while the GPU may still read it.
    lease: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    linearize: bool,
    planes: Option<Planes>,
    /// Bound in the unused V slot for NV12 (the shader ignores it).
    placeholder: wgpu::Texture,
    egui: egui_wgpu::Renderer,
    #[cfg(target_os = "macos")]
    metal: Option<crate::render_metal::MetalImport>,
    #[cfg(target_os = "windows")]
    d3d12: Option<crate::render_d3d12::D3d12Import>,
}

impl Renderer {
    /// `vsync: false` presents immediately instead of at the next display refresh: up to one
    /// refresh less latency, but the picture can tear (most visibly in full screen).
    pub fn new(window: Arc<Window>, vsync: bool) -> Result<Self> {
        let (surface, adapter) = pick_adapter(&window)?;
        // Zero copy on Windows imports the decoder's NV12 textures.
        #[cfg(target_os = "windows")]
        let required_features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_NV12;
        #[cfg(not(target_os = "windows"))]
        let required_features = wgpu::Features::empty();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("freemonplay"),
            required_features,
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))?;
        tracing::info!("rendering with {:?} on {}", adapter.get_info().backend, adapter.get_info().name);

        let caps = surface.get_capabilities(&adapter);
        // Video is already gamma encoded, so prefer a non-sRGB surface and write it as is.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        tracing::debug!("present modes available: {:?}", caps.present_modes);
        let present_mode = if !vsync && caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            // Still waits for vsync, but never blocks us behind an older frame.
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        tracing::debug!("using present mode {present_mode:?}");
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: Default::default(),
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let linearize = format.is_srgb();
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("params"),
            contents: bytemuck::bytes_of(&Params { scale: [1.0, 1.0], linearize: linearize as u32 as f32, nv12: 0.0 }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let placeholder = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("placeholder"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        #[cfg(target_os = "macos")]
        let metal = crate::render_metal::MetalImport::new(&device);
        #[cfg(target_os = "windows")]
        let d3d12 = crate::render_d3d12::D3d12Import::new(&device, &adapter);
        let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            params,
            linearize,
            planes: None,
            placeholder,
            egui,
            #[cfg(target_os = "macos")]
            metal,
            #[cfg(target_os = "windows")]
            d3d12,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn upload(&mut self, frame: &YuvFrame) {
        #[cfg(target_os = "windows")]
        if let decode::Planes::Shared(shared) = &frame.planes {
            // Zero copy: draw the decoder's shared texture through views of its two planes.
            let imported = match &mut self.d3d12 {
                Some(d3d12) => d3d12.import(&self.device, shared, frame.width, frame.height),
                None => Err(anyhow::anyhow!("no Direct3D 12 import")),
            };
            match imported {
                Ok(texture) => {
                    let lease: Arc<dyn std::any::Any + Send + Sync> = shared.lease.clone();
                    self.planes = Some(self.bind_imported_nv12(frame.width, frame.height, texture, lease));
                }
                Err(e) => {
                    // The decoder switches to copied frames; this one is skipped.
                    tracing::warn!("zero-copy display failed ({e:#}); copying frames from now on");
                    shared.renderer_gave_up.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.d3d12 = None;
                }
            }
            return;
        }
        #[cfg(target_os = "macos")]
        if let decode::Planes::Native(buffer) = &frame.planes {
            // Zero copy: draw straight from the decoder's buffer.
            if let Some(metal) = &self.metal {
                match metal.import(&self.device, buffer) {
                    Ok((y, uv)) => {
                        self.planes = Some(self.bind_planes(frame.width, frame.height, true, y, uv, None));
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("zero-copy display failed ({e:#}); copying frames from now on");
                        self.metal = None;
                    }
                }
            }
            if let Some(planes) = buffer.to_nv12() {
                let copied = YuvFrame { planes, received_at: frame.received_at, ..*frame };
                self.upload(&copied);
            }
            return;
        }
        let nv12 = matches!(frame.planes, decode::Planes::Nv12 { .. });
        if self.planes.as_ref().is_none_or(|p| !p.owned || p.width != frame.width || p.height != frame.height || p.nv12 != nv12) {
            self.planes = Some(self.make_planes(frame.width, frame.height, nv12));
        }
        let planes = self.planes.as_ref().unwrap();
        let (cw, ch) = (frame.width.div_ceil(2), frame.height.div_ceil(2));
        match &frame.planes {
            decode::Planes::I420 { y, u, v, y_stride, uv_stride } => {
                self.write_plane(&planes.y, y, *y_stride, frame.width, frame.height);
                self.write_plane(&planes.u, u, *uv_stride, cw, ch);
                self.write_plane(planes.v.as_ref().expect("I420 has a V plane"), v, *uv_stride, cw, ch);
            }
            decode::Planes::Nv12 { y, uv, y_stride, uv_stride } => {
                self.write_plane(&planes.y, y, *y_stride, frame.width, frame.height);
                self.write_plane(&planes.u, uv, *uv_stride, cw, ch);
            }
            #[cfg(target_os = "macos")]
            decode::Planes::Native(_) => unreachable!("handled above"),
            #[cfg(target_os = "windows")]
            decode::Planes::Shared(_) => unreachable!("handled above"),
        }
    }

    /// The GPU the renderer runs on, for a decoder to share frames with (Windows: the
    /// adapter LUID). `None` where zero copy doesn't need it or isn't available.
    pub fn gpu_adapter(&self) -> Option<u64> {
        #[cfg(target_os = "windows")]
        return self.d3d12.as_ref().map(crate::render_d3d12::D3d12Import::adapter_luid);
        #[cfg(not(target_os = "windows"))]
        return None;
    }

    /// Largest texture egui may create.
    pub fn max_texture_side(&self) -> usize {
        self.device.limits().max_texture_dimension_2d as usize
    }

    /// Forgets the last video frame (when going back to the host list).
    pub fn clear_video(&mut self) {
        self.planes = None;
    }

    /// Draws the latest video frame, if any, with the interface on top, and presents.
    pub fn render(&mut self, ui: Option<UiFrame>) {
        // egui's texture changes must be applied even if this frame can't be shown: the first
        // set holds the whole font atlas, and later ones only patch it.
        if let Some(ui) = &ui {
            for (id, deltas) in &ui.textures.set {
                for delta in deltas {
                    self.egui.update_texture(&self.device, &self.queue, *id, delta);
                }
            }
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.free_ui_textures(ui.as_ref());
                return;
            }
            _ => {
                self.free_ui_textures(ui.as_ref());
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

        if let Some(p) = &self.planes {
            let (sx, sy) = letterbox_scale((p.width, p.height), (self.config.width, self.config.height));
            let params =
                Params { scale: [sx, sy], linearize: self.linearize as u32 as f32, nv12: p.nv12 as u32 as f32 };
            self.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: ui.as_ref().map_or(1.0, |u| u.pixels_per_point),
        };
        let mut ui_commands = Vec::new();
        if let Some(ui) = &ui {
            ui_commands = self.egui.update_buffers(&self.device, &self.queue, &mut encoder, &ui.primitives, &screen);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            if let Some(p) = &self.planes {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &p.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            if let Some(ui) = &ui {
                self.egui.render(&mut pass.forget_lifetime(), &ui.primitives, &screen);
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(d3d12) = &mut self.d3d12 {
            d3d12.before_submit(&self.queue);
        }
        self.queue.submit(ui_commands.into_iter().chain([encoder.finish()]));
        // A decoder's buffer stays reserved until the GPU has finished reading it.
        if let Some(lease) = self.planes.as_ref().and_then(|p| p.lease.clone()) {
            self.queue.on_submitted_work_done(move || drop(lease));
            let _ = self.device.poll(wgpu::PollType::Poll);
        }
        self.queue.present(frame);
        self.free_ui_textures(ui.as_ref());
    }

    fn free_ui_textures(&mut self, ui: Option<&UiFrame>) {
        if let Some(ui) = ui {
            for id in &ui.textures.free {
                self.egui.free_texture(id);
            }
        }
    }

    fn make_planes(&self, width: u32, height: u32, nv12: bool) -> Planes {
        let tex = |label, w: u32, h: u32, format| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let r8 = wgpu::TextureFormat::R8Unorm;
        let (y, u, v) = if nv12 {
            (tex("y", width, height, r8), tex("uv", cw, ch, wgpu::TextureFormat::Rg8Unorm), None)
        } else {
            (tex("y", width, height, r8), tex("u", cw, ch, r8), Some(tex("v", cw, ch, r8)))
        };
        self.bind_planes(width, height, nv12, y, u, v)
    }

    fn bind_planes(
        &self,
        width: u32,
        height: u32,
        nv12: bool,
        y: wgpu::Texture,
        u: wgpu::Texture,
        v: Option<wgpu::Texture>,
    ) -> Planes {
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (vy, vu, vv) = (view(&y), view(&u), view(v.as_ref().unwrap_or(&self.placeholder)));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&vy) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vu) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&vv) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: self.params.as_entire_binding() },
            ],
        });
        Planes { width, height, nv12, owned: true, y, u, v, bind_group, lease: None }
    }

    /// A decoder's NV12 texture, sampled through views of its luma and chroma planes.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    fn bind_imported_nv12(
        &self,
        width: u32,
        height: u32,
        texture: wgpu::Texture,
        lease: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Planes {
        let plane = |format, aspect| {
            texture.create_view(&wgpu::TextureViewDescriptor { format: Some(format), aspect, ..Default::default() })
        };
        let vy = plane(wgpu::TextureFormat::R8Unorm, wgpu::TextureAspect::Plane0);
        let vuv = plane(wgpu::TextureFormat::Rg8Unorm, wgpu::TextureAspect::Plane1);
        let vv = self.placeholder.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv (shared)"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&vy) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vuv) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&vv) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: self.params.as_entire_binding() },
            ],
        });
        Planes { width, height, nv12: true, owned: false, y: texture.clone(), u: texture, v: None, bind_group, lease: Some(lease) }
    }

    fn write_plane(&self, texture: &wgpu::Texture, data: &[u8], stride: u32, width: u32, height: u32) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }
}

/// How much of the window (in clip space, 0..1 per axis) the video covers.
pub fn letterbox_scale(video: (u32, u32), window: (u32, u32)) -> (f32, f32) {
    let va = video.0 as f32 / video.1 as f32;
    let wa = window.0 as f32 / window.1.max(1) as f32;
    if va > wa { (1.0, wa / va) } else { (va / wa, 1.0) }
}

/// The surface and GPU adapter to render with. On Windows, Direct3D 12 first: zero-copy
/// display shares textures with the Direct3D-based decoder.
fn pick_adapter(window: &Arc<Window>) -> Result<(wgpu::Surface<'static>, wgpu::Adapter)> {
    let try_backends = |backends: wgpu::Backends| -> Result<(wgpu::Surface<'static>, wgpu::Adapter)> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no usable GPU adapter")?;
        Ok((surface, adapter))
    };
    #[cfg(target_os = "windows")]
    if let Ok(found) = try_backends(wgpu::Backends::DX12) {
        return Ok(found);
    }
    try_backends(wgpu::Backends::all())
}
