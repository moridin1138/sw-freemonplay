//! Captures a monitor with Windows.Graphics.Capture (cursor included, like ScreenCaptureKit
//! on macOS), then scales it to the stream size and converts it to NV12 (BT.709, limited
//! range) in one pass on the GPU's video processor.
//!
//! Frames stay on the GPU: the hardware encoder takes the NV12 texture directly. Each
//! texture comes from a small ring and is leased to its frame, so capture never overwrites
//! a picture the encoder is still reading.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext1, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{Interface, factory};

use super::VirtualDisplay;
use super::d3d;
use crate::frame::{Frame, FrameSlot, I420Frame};

/// NV12 textures per capture: shown, encoding, being written, and a spare.
const RING: usize = 4;

/// A captured picture: an NV12 texture on the shared device.
pub struct NativeFrame {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub captured_at: Instant,
    /// Keeps the texture out of the capture ring until this frame is dropped.
    _lease: Arc<()>,
}

// SAFETY: a texture on the multithread-protected shared device; only read after capture.
unsafe impl Send for NativeFrame {}
unsafe impl Sync for NativeFrame {}

impl NativeFrame {
    /// Reads the picture back as packed I420 (for the software encoder).
    pub fn to_i420(&self) -> Option<I420Frame> {
        let d = d3d::shared().ok()?;
        let staging = texture(&d.device, self.width, self.height, DXGI_FORMAT_NV12, Kind::Staging).ok()?;
        let (w, h) = (self.width as usize, self.height as usize);
        // SAFETY: a GPU copy into our staging texture, then a mapped read within its pitch
        // and height.
        unsafe {
            d.context.CopyResource(&staging, &self.texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            d.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let pitch = mapped.RowPitch as usize;
            let base = mapped.pData as *const u8;
            // Staging NV12: the CbCr plane follows the Y plane at pitch * height.
            let y = std::slice::from_raw_parts(base, pitch * h);
            let uv = std::slice::from_raw_parts(base.add(pitch * h), pitch * h / 2);
            let mut picture = I420Frame::from_nv12(w, h, y, pitch, uv, pitch);
            d.context.Unmap(&staging, 0);
            picture.captured_at = self.captured_at;
            Some(picture)
        }
    }
}

pub struct Capture {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    /// The FrameArrived registration, removed before closing so no callback runs after.
    token: i64,
}

// SAFETY: WinRT capture objects are agile (usable from any thread); we only close them.
unsafe impl Send for Capture {}

impl Capture {
    pub fn start(display: &VirtualDisplay, width: u32, height: u32, fps: u32, slot: Arc<FrameSlot>) -> Result<Self> {
        let d = d3d::shared()?;
        // SAFETY: standard WinRT/D3D interop; all handles are valid for the calls.
        let (item, rt_device) = unsafe {
            let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            // A monitor that has just appeared may not be capturable yet; retry briefly,
            // looking its handle up again each time.
            let deadline = Instant::now() + Duration::from_secs(3);
            let item: GraphicsCaptureItem = loop {
                match interop.CreateForMonitor(display.current_monitor()) {
                    Ok(item) => break item,
                    Err(e) if Instant::now() < deadline => {
                        tracing::debug!("monitor not capturable yet ({e}); retrying");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e) => return Err(anyhow::Error::from(e).context("can't capture that monitor")),
                }
            };
            let rt_device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&d.device.cast::<IDXGIDevice>()?)?.cast()?;
            (item, rt_device)
        };
        let size = item.Size()?;
        let (source_w, source_h) = (size.Width as u32, size.Height as u32);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&rt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        // Windows 11 can drop the yellow "being captured" border; the cursor is drawn in.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);

        let converter = Mutex::new(Converter::new(&d, (source_w, source_h), (width, height), fps)?);
        let got_frame = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let got = got_frame.clone();
        let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64).mul_f32(0.9);
        let last = Mutex::new(None::<Instant>);
        let warned = std::sync::atomic::AtomicBool::new(false);
        let token = pool.FrameArrived(&TypedEventHandler::new(move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
            let Some(pool) = pool.as_ref() else { return Ok(()) };
            let frame = pool.TryGetNextFrame()?;
            got.store(true, std::sync::atomic::Ordering::Relaxed);
            let captured_at = Instant::now();
            // Windows delivers as fast as the screen changes; stick to the stream's rate.
            {
                let mut last = last.lock().unwrap();
                if last.is_some_and(|t| captured_at - t < interval) {
                    return Ok(());
                }
                *last = Some(captured_at);
            }
            // SAFETY: the frame's surface is a D3D11 texture on our device.
            let texture: ID3D11Texture2D = unsafe { frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()? };
            match converter.lock().unwrap().convert(&texture, captured_at) {
                Ok(Some(picture)) => slot.push(Frame::Native(picture)),
                // Every texture is still in use: the encoder is behind, so skip this one.
                Ok(None) => {}
                Err(e) => {
                    if !warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        tracing::warn!("couldn't convert a captured frame: {e:#}");
                    }
                }
            }
            Ok(())
        }))?;
        session.StartCapture()?;
        tracing::info!("capturing {source_w}x{source_h}, streaming {width}x{height}@{fps}");
        // Windows captures only when the desktop is redrawn, and a new, empty monitor may not
        // be for a while. Ask for a repaint if the first frame is slow to come.
        std::thread::spawn(move || {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(300));
                if got_frame.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                // SAFETY: invalidating every window so the desktop is composed again.
                let _ = unsafe { RedrawWindow(None, None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME) };
            }
        });
        Ok(Self { session, pool, token })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Stop callbacks first: one arriving while (or after) the session closes could run in
        // a capture library Windows is unloading.
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

enum Kind {
    /// Our copy of a captured picture, read by the video processor.
    Input,
    /// Video processor output, read by the encoder.
    Output,
    /// CPU readable.
    Staging,
}

fn texture(device: &ID3D11Device, width: u32, height: u32, format: DXGI_FORMAT, kind: Kind) -> Result<ID3D11Texture2D> {
    let (usage, bind, cpu) = match kind {
        Kind::Input => (D3D11_USAGE_DEFAULT, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32, 0),
        // NV12 can't be a shader resource; render target only.
        Kind::Output => (D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0),
        Kind::Staging => (D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32),
    };
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut t = None;
    // SAFETY: a valid description and out pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.context("no texture")
}

struct Target {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    lease: Arc<()>,
}

/// Scales and converts BGRA to NV12 with the GPU's video processor.
struct Converter {
    context: ID3D11DeviceContext,
    video: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    /// Our copy of the captured picture (capture textures are recycled by the frame pool).
    input: ID3D11Texture2D,
    input_view: ID3D11VideoProcessorInputView,
    ring: Vec<Target>,
    size: (u32, u32),
    source: (u32, u32),
}

// SAFETY: D3D11 objects on the multithread-protected shared device; used under a mutex.
unsafe impl Send for Converter {}

impl Converter {
    fn new(d: &d3d::Device, source: (u32, u32), size: (u32, u32), fps: u32) -> Result<Self> {
        if !size.0.is_multiple_of(2) || !size.1.is_multiple_of(2) {
            bail!("NV12 needs an even size, got {}x{}", size.0, size.1);
        }
        let video_device: ID3D11VideoDevice = d.device.cast()?;
        let video: ID3D11VideoContext1 = d.context.cast()?;
        let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: source.0,
            InputHeight: source.1,
            OutputFrameRate: rate,
            OutputWidth: size.0,
            OutputHeight: size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let input = texture(&d.device, source.0, source.1, DXGI_FORMAT_B8G8R8A8_UNORM, Kind::Input)?;
        // SAFETY: creating video processor objects from valid descriptions and resources.
        unsafe {
            let enumerator: ID3D11VideoProcessorEnumerator = video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut input_view = None;
            video_device.CreateVideoProcessorInputView(&input, &enumerator, &in_desc, Some(&mut input_view))?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut ring = Vec::with_capacity(RING);
            for _ in 0..RING {
                let texture = texture(&d.device, size.0, size.1, DXGI_FORMAT_NV12, Kind::Output)?;
                let mut view = None;
                video_device.CreateVideoProcessorOutputView(&texture, &enumerator, &out_desc, Some(&mut view))?;
                ring.push(Target { texture, view: view.context("no output view")?, lease: Arc::new(()) });
            }
            // Desktop pixels are full range RGB; clients expect BT.709 limited range video.
            video.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            video.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            video.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            Ok(Self {
                context: d.context.clone(),
                video,
                processor,
                input,
                input_view: input_view.context("no input view")?,
                ring,
                size,
                source,
            })
        }
    }

    /// `None` when every ring texture is still held by a frame.
    fn convert(&mut self, captured: &ID3D11Texture2D, captured_at: Instant) -> Result<Option<NativeFrame>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain query.
        unsafe { captured.GetDesc(&mut desc) };
        if (desc.Width, desc.Height) != self.source {
            bail!("the monitor's size changed ({}x{}); reconnect to pick it up", desc.Width, desc.Height);
        }
        let Some(target) = self.ring.iter().find(|t| Arc::strong_count(&t.lease) == 1) else { return Ok(None) };
        // SAFETY: a GPU copy and a blit between resources we own.
        unsafe {
            self.context.CopyResource(&self.input, captured);
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
                ..Default::default()
            };
            let streams = [stream];
            let blit = self.video.VideoProcessorBlt(&self.processor, &target.view, 0, &streams);
            let [stream] = streams;
            drop(ManuallyDrop::into_inner(stream.pInputSurface));
            blit?;
            self.context.Flush();
        }
        Ok(Some(NativeFrame {
            texture: target.texture.clone(),
            width: self.size.0,
            height: self.size.1,
            captured_at,
            _lease: target.lease.clone(),
        }))
    }
}
