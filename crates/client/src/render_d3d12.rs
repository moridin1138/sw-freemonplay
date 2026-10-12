//! Zero-copy display on Windows: Media Foundation decodes on the GPU and copies each frame
//! (GPU to GPU) into a shared NV12 texture (see decode/mediafoundation.rs). We open those
//! textures and the shared fence in wgpu's Direct3D 12 device, so frames never pass through
//! system memory.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};

use crate::decode::mediafoundation::SharedFrame;

type Dx12 = wgpu::hal::api::Dx12;

pub struct D3d12Import {
    device: ID3D12Device,
    /// Opened fences, by the decoder pool that owns them.
    fences: HashMap<u64, ID3D12Fence>,
    /// Opened textures, by (pool, slot).
    textures: HashMap<(u64, usize), wgpu::Texture>,
    /// Make the GPU wait for this before drawing the next frame.
    pending_wait: Option<(ID3D12Fence, u64)>,
}

impl D3d12Import {
    /// `None` unless wgpu runs on Direct3D 12 with NV12 support (or FMP_ZERO_COPY=0).
    pub fn new(device: &wgpu::Device, adapter: &wgpu::Adapter) -> Option<Self> {
        if std::env::var("FMP_ZERO_COPY").is_ok_and(|v| v == "0") {
            tracing::info!("zero-copy display disabled by FMP_ZERO_COPY=0");
            return None;
        }
        if adapter.get_info().backend != wgpu::Backend::Dx12 || !device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
            tracing::info!("zero-copy display needs Direct3D 12 with NV12 textures; copying frames instead");
            return None;
        }
        // SAFETY: we only read the raw device to open shared handles on it.
        let raw = unsafe { device.as_hal::<Dx12>() }?.raw_device().clone();
        Some(Self { device: raw, fences: HashMap::new(), textures: HashMap::new(), pending_wait: None })
    }

    /// The GPU, for the decoder to run on (shared textures only work within one GPU).
    pub fn adapter_luid(&self) -> u64 {
        // SAFETY: a plain query.
        let luid = unsafe { self.device.GetAdapterLuid() };
        ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
    }

    /// Returns the wgpu texture for a shared frame, opening it on first use, and arranges
    /// for the GPU to wait for the decoder's copy before reading it.
    pub fn import(&mut self, device: &wgpu::Device, frame: &SharedFrame, width: u32, height: u32) -> Result<wgpu::Texture> {
        let result = self.import_inner(device, frame, width, height);
        if result.is_err() {
            // Tell the decoder to stop sharing and send copied frames instead.
            frame.renderer_gave_up.store(true, Ordering::Relaxed);
        }
        result
    }

    fn import_inner(&mut self, device: &wgpu::Device, frame: &SharedFrame, width: u32, height: u32) -> Result<wgpu::Texture> {
        // Forget anything from older pools (a new session, or a resolution change).
        self.textures.retain(|(ring, _), _| *ring == frame.ring_id);
        self.fences.retain(|ring, _| *ring == frame.ring_id);

        if !self.fences.contains_key(&frame.ring_id) {
            let mut fence: Option<ID3D12Fence> = None;
            // SAFETY: the decoder keeps the handle open while this frame exists.
            unsafe { self.device.OpenSharedHandle(frame.fence, &mut fence) }.context("opening the decoder's fence")?;
            self.fences.insert(frame.ring_id, fence.context("no fence")?);
        }
        let key = (frame.ring_id, frame.slot);
        if !self.textures.contains_key(&key) {
            let mut resource: Option<ID3D12Resource> = None;
            // SAFETY: as above.
            unsafe { self.device.OpenSharedHandle(frame.texture, &mut resource) }.context("opening a shared texture")?;
            let resource = resource.context("no texture")?;
            let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
            // SAFETY: the resource is a 2D, single-mip NV12 texture of exactly this size,
            // created by the decoder on this same GPU.
            let texture = unsafe {
                let hal = wgpu::hal::dx12::Device::texture_from_raw(
                    resource,
                    wgpu::TextureFormat::NV12,
                    wgpu::TextureDimension::D2,
                    size,
                    1,
                    1,
                );
                device.create_texture_from_hal::<Dx12>(
                    hal,
                    &wgpu::TextureDescriptor {
                        label: Some("shared decoded frame"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::NV12,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::TextureUses::RESOURCE,
                )
            };
            self.textures.insert(key, texture);
        }
        let Some(fence) = self.fences.get(&frame.ring_id) else { bail!("fence vanished") };
        self.pending_wait = Some((fence.clone(), frame.fence_value));
        Ok(self.textures[&key].clone())
    }

    /// Call right before submitting the frame's draw: makes wgpu's GPU queue wait until the
    /// decoder's copy into the shared texture has finished.
    pub fn before_submit(&mut self, queue: &wgpu::Queue) {
        let Some((fence, value)) = self.pending_wait.take() else { return };
        // SAFETY: queueing a GPU-side wait on wgpu's own command queue, ahead of its submit.
        if let Some(hal) = unsafe { queue.as_hal::<Dx12>() }
            && let Err(e) = unsafe { hal.as_raw().Wait(&fence, value) }
        {
            tracing::warn!("couldn't wait for the decoder's copy: {e}");
        }
    }
}
