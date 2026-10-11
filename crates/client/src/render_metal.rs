//! Zero-copy display on macOS: VideoToolbox decodes into IOSurface-backed pixel buffers,
//! which CoreVideo's Metal texture cache can expose as textures without copying. We hand
//! those to wgpu through its Metal backend.

use std::ptr::NonNull;

use anyhow::{Context, Result, bail};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane,
};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTextureType};

use crate::decode::videotoolbox::PixelBuffer;

pub struct MetalImport {
    cache: CFRetained<CVMetalTextureCache>,
}

/// Keeps the decoder's buffer and the texture wrapping it alive until wgpu has finished
/// with the texture on the GPU (wgpu calls the drop callback then).
struct KeepAlive {
    _texture: CFRetained<CVMetalTexture>,
    _buffer: PixelBuffer,
}

// SAFETY: both are reference-counted CF objects that are only released here.
unsafe impl Send for KeepAlive {}
unsafe impl Sync for KeepAlive {}

impl MetalImport {
    /// Returns `None` when the device isn't Metal (it always is on macOS) or zero-copy was
    /// switched off with FMP_ZERO_COPY=0.
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        if std::env::var("FMP_ZERO_COPY").is_ok_and(|v| v == "0") {
            tracing::info!("zero-copy display disabled by FMP_ZERO_COPY=0");
            return None;
        }
        // SAFETY: we only read the raw MTLDevice to create a texture cache on it.
        let mtl: Retained<ProtocolObject<dyn MTLDevice>> =
            unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?.raw_device().clone();
        let mut raw: *mut CVMetalTextureCache = std::ptr::null_mut();
        // SAFETY: creating a texture cache for a valid Metal device.
        let status = unsafe { CVMetalTextureCache::create(None, None, &mtl, None, NonNull::from(&mut raw)) };
        if status != 0 {
            tracing::warn!("CVMetalTextureCacheCreate failed ({status}); copying frames instead");
            return None;
        }
        // SAFETY: Create returned +1 ownership.
        let cache = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        tracing::info!("zero-copy display enabled");
        Some(Self { cache })
    }

    /// Wraps a decoded NV12 buffer's luma and chroma planes as wgpu textures, sharing the
    /// decoder's memory.
    pub fn import(&self, device: &wgpu::Device, buffer: &PixelBuffer) -> Result<(wgpu::Texture, wgpu::Texture)> {
        // Lets the cache release textures nobody holds any more.
        self.cache.flush(0);
        let y = self.plane(device, buffer, 0, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?;
        let uv = self.plane(device, buffer, 1, MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm)?;
        Ok((y, uv))
    }

    fn plane(
        &self,
        device: &wgpu::Device,
        buffer: &PixelBuffer,
        plane: usize,
        mtl_format: MTLPixelFormat,
        format: wgpu::TextureFormat,
    ) -> Result<wgpu::Texture> {
        let pb = &*buffer.0;
        let (width, height) = (CVPixelBufferGetWidthOfPlane(pb, plane), CVPixelBufferGetHeightOfPlane(pb, plane));
        let mut raw: *mut CVMetalTexture = std::ptr::null_mut();
        // SAFETY: the buffer is IOSurface-backed and Metal compatible (the decoder asks for
        // that); the plane index and size come from the buffer itself.
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache,
                pb,
                None,
                mtl_format,
                width,
                height,
                plane,
                NonNull::from(&mut raw),
            )
        };
        if status != 0 {
            bail!("CVMetalTextureCacheCreateTextureFromImage failed for plane {plane} ({status})");
        }
        // SAFETY: Create returned +1 ownership.
        let cv_texture = unsafe { CFRetained::from_raw(NonNull::new(raw).context("null CVMetalTexture")?) };
        let mtl_texture = CVMetalTextureGetTexture(&cv_texture).context("CVMetalTexture has no MTLTexture")?;

        let keep_alive = KeepAlive { _texture: cv_texture, _buffer: buffer.clone() };
        let (w, h) = (width as u32, height as u32);
        // SAFETY: the MTLTexture is a valid 2D, single-mip texture of exactly this format and
        // size, created on wgpu's own device; KeepAlive holds its backing memory until wgpu
        // drops the texture.
        let hal_texture = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                mtl_texture,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width: w, height: h, depth: 1 },
                Some(Box::new(move || drop(keep_alive))),
            )
        };
        // SAFETY: as above; the texture is only ever sampled.
        let texture = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("decoded plane"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        Ok(texture)
    }
}
