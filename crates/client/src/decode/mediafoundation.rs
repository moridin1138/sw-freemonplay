//! H.264 / HEVC decoding with Windows Media Foundation's built-in decoders.
//!
//! Given a D3D11 device the decoder uses DXVA, i.e. the GPU's fixed-function video
//! decoder, which keeps CPU use tiny even on low-power chips. Without a usable GPU it
//! still works, decoding in software inside Windows' own (licensed) decoder.
//!
//! When the renderer shares its GPU, decoded frames are copied GPU-to-GPU into a small pool
//! of shared textures and drawn from there, never touching system memory (zero copy).
//!
//! Windows "N" editions ship without Media Foundation; installing Microsoft's Media
//! Feature Pack adds it. The HEVC decoder only exists once Microsoft's "HEVC Video
//! Extensions" are installed (often preinstalled by PC makers); without them we use H.264.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use fmp_proto::Codec;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FENCE_FLAG_SHARED,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4,
    ID3D11Fence, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIFactory4,
    IDXGIResource1,
};
use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::Interface;

use super::{Decoder, Deliver, Planes, YuvFrame};

/// MF_VERSION from mfapi.h: (MF_SDK_VERSION << 16) | MF_API_VERSION.
const MF_VERSION: u32 = (0x0002 << 16) | 0x0070;

pub fn supported_codecs() -> Vec<Codec> {
    // SAFETY: probing whether the HEVC decoder is registered; the instance is dropped at once.
    let hevc = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance::<_, IMFTransform>(&CLSID_MSH265DecoderMFT, None, CLSCTX_INPROC_SERVER).is_ok()
    };
    if hevc { vec![Codec::H265, Codec::H264] } else { vec![Codec::H264] }
}

pub struct MfDecoder {
    transform: IMFTransform,
    codec: Codec,
    gpu: bool,
    /// Size of the decoder's output surfaces (rounded up to macroblocks, e.g. 1088 rows).
    coded: (u32, u32),
    /// The part that is actual picture (e.g. 1080 rows).
    visible: (u32, u32),
    default_stride: u32,
    deliver: Deliver,
    // Kept alive for as long as the decoder uses it.
    _device_manager: Option<IMFDXGIDeviceManager>,
    /// The D3D11 device the decoder uses (GPU mode).
    device: Option<ID3D11Device>,
    /// The renderer's GPU, when zero copy is possible.
    share_with: Option<u64>,
    ring: Option<SharedRing>,
}

impl MfDecoder {
    /// `gpu_adapter` is the renderer's GPU (adapter LUID); when given, the decoder runs on
    /// that GPU and shares frames with the renderer instead of copying them.
    pub fn new(codec: Codec, deliver: Deliver, gpu_adapter: Option<u64>) -> Result<Self> {
        // SAFETY: per-thread COM init and refcounted MF startup; this thread owns the decoder.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("Media Foundation is missing (Windows N? install the Media Feature Pack)")?;
        }
        match Self::create(codec, deliver.clone(), true, gpu_adapter) {
            Ok(d) => Ok(d),
            Err(e) => {
                tracing::warn!("GPU decoding unavailable ({e:#}); using Windows' software decoder");
                Self::create(codec, deliver, false, None)
            }
        }
    }

    fn create(codec: Codec, deliver: Deliver, gpu: bool, share_with: Option<u64>) -> Result<Self> {
        let (clsid, subtype) = match codec {
            Codec::H264 => (CLSID_MSH264DecoderMFT, MFVideoFormat_H264),
            Codec::H265 => (CLSID_MSH265DecoderMFT, MFVideoFormat_HEVC),
        };
        // SAFETY: standard MFT setup; all COM objects are owned by the returned decoder.
        unsafe {
            let transform: IMFTransform = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)
                .with_context(|| format!("Windows {codec} decoder not found"))?;
            let attributes = transform.GetAttributes()?;
            // Output each frame as soon as it's decoded instead of buffering for reordering.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;

            let (device_manager, device) = if gpu {
                if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                    bail!("decoder isn't D3D11 aware");
                }
                let (manager, device) = d3d11_device_manager(share_with)?;
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
                (Some(manager), Some(device))
            } else {
                (None, None)
            };

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Self {
                transform,
                codec,
                gpu,
                coded: (0, 0),
                visible: (0, 0),
                default_stride: 0,
                deliver,
                _device_manager: device_manager,
                device,
                share_with,
                ring: None,
            };
            decoder.choose_output_type()?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Picks NV12 output. Called at start and whenever the stream format changes.
    fn choose_output_type(&mut self) -> Result<()> {
        // SAFETY: enumerating and setting media types on our transform.
        unsafe {
            for i in 0.. {
                let Ok(t) = self.transform.GetOutputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                    continue;
                }
                self.transform.SetOutputType(0, &t, 0)?;
                let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                self.coded = ((size >> 32) as u32, size as u32);
                self.visible = display_aperture(&t).unwrap_or(self.coded);
                self.default_stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(self.coded.0);
                return Ok(());
            }
        }
        bail!("decoder offers no NV12 output")
    }

    /// Pulls every picture the decoder has ready.
    fn drain(&mut self) -> Result<()> {
        loop {
            // SAFETY: ProcessOutput per the MFT contract. We provide a sample only when the
            // decoder doesn't allocate its own, and take ownership of whatever it returns.
            unsafe {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
                let ours = if info.dwFlags & provides == 0 {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize)?)?;
                    Some(sample)
                } else {
                    None
                };
                let mut out = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(ours),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut out, &mut status);
                let sample = ManuallyDrop::take(&mut out[0].pSample);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                match result {
                    Ok(()) => {
                        if let Some(sample) = sample {
                            // Zero copy when we can; otherwise read the picture back to memory.
                            let shared = self.share(&sample).unwrap_or_else(|e| {
                                tracing::warn!("zero-copy hand-off failed ({e:#}); copying frames instead");
                                self.share_with = None;
                                None
                            });
                            let picture = match shared {
                                Some(picture) => Some(picture),
                                None => self.read_nv12(&sample)?,
                            };
                            if let Some(picture) = picture {
                                (self.deliver)(picture);
                            }
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output_type()?,
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    /// Hands a decoded GPU frame to the renderer through a shared texture, GPU to GPU.
    /// `None` when sharing isn't possible right now; the caller then copies instead.
    fn share(&mut self, sample: &IMFSample) -> Result<Option<YuvFrame>> {
        let (Some(_), Some(device)) = (self.share_with, self.device.clone()) else { return Ok(None) };
        if self.ring.as_ref().is_some_and(|r| r.renderer_gave_up.load(Ordering::Relaxed)) {
            tracing::warn!("renderer couldn't use shared frames; copying frames instead");
            self.share_with = None;
            self.ring = None;
            return Ok(None);
        }
        let (w, h) = self.visible;
        if w == 0 || h == 0 {
            return Ok(None);
        }
        // SAFETY: reading the decoder's own output texture; the copy is queued on the same
        // (multithread protected) device the decoder uses.
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            // Software decoding gives plain memory buffers: nothing to share.
            let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() else { return Ok(None) };
            let mut raw = null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
            let source = ID3D11Texture2D::from_raw(raw);
            let subresource = dxgi.GetSubresourceIndex()?;

            if self.ring.as_ref().is_none_or(|r| r.size != (w, h)) {
                self.ring = Some(SharedRing::new(&device, w, h)?);
            }
            let ring = self.ring.as_mut().expect("just created");
            let Some(slot) = ring.free_slot() else {
                tracing::debug!("all shared textures busy; copying this frame");
                return Ok(None);
            };
            let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            ring.context.CopySubresourceRegion(&ring.textures[slot], 0, 0, 0, 0, &source, subresource, Some(&region));
            ring.fence_value += 1;
            ring.context4.Signal(&ring.fence, ring.fence_value)?;
            ring.context.Flush();
            Ok(Some(YuvFrame {
                width: w,
                height: h,
                planes: Planes::Shared(SharedFrame {
                    ring_id: ring.id,
                    slot,
                    texture: ring.handles.textures[slot],
                    fence: ring.handles.fence,
                    fence_value: ring.fence_value,
                    renderer_gave_up: ring.renderer_gave_up.clone(),
                    _handles: ring.handles.clone(),
                    lease: Arc::new(SlotLease(ring.busy[slot].clone())),
                }),
                // We set sample times to the host's timestamp (in 100 ns units) on input.
                pts_us: (sample.GetSampleTime().unwrap_or(0) / 10).max(0) as u64,
                received_at: None,
            }))
        }
    }

    /// Copies an output sample's NV12 planes into system memory. For GPU surfaces,
    /// Lock2D maps (copies back) the decoded texture for us.
    unsafe fn read_nv12(&self, sample: &IMFSample) -> Result<Option<YuvFrame>> {
        let (w, h) = self.visible;
        let coded_h = self.coded.1.max(h) as usize;
        if w == 0 || h == 0 {
            return Ok(None);
        }
        // SAFETY: locked reads within the plane sizes MF reports, unlocked afterwards.
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            // We set sample times to the host's timestamp (in 100 ns units) on input.
            let pts_us = (sample.GetSampleTime().unwrap_or(0) / 10).max(0) as u64;
            let copy = |base: *const u8, pitch: usize| {
                let y = std::slice::from_raw_parts(base, pitch * h as usize).to_vec();
                let uv = std::slice::from_raw_parts(base.add(pitch * coded_h), pitch * h.div_ceil(2) as usize).to_vec();
                YuvFrame {
                    width: w,
                    height: h,
                    planes: Planes::Nv12 { y, uv, y_stride: pitch as u32, uv_stride: pitch as u32 },
                    pts_us,
                    received_at: None,
                }
            };
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                let mut base = null_mut();
                let mut pitch = 0i32;
                buffer2d.Lock2D(&mut base, &mut pitch)?;
                let frame = (pitch > 0).then(|| copy(base, pitch as usize));
                buffer2d.Unlock2D()?;
                Ok(frame)
            } else {
                let mut base = null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut base, None, Some(&mut len))?;
                let pitch = self.default_stride.max(w) as usize;
                let needed = pitch * (coded_h + h.div_ceil(2) as usize);
                let frame = (len as usize >= needed).then(|| copy(base, pitch));
                buffer.Unlock()?;
                Ok(frame)
            }
        }
    }
}

impl Decoder for MfDecoder {
    fn name(&self) -> String {
        let kind = if self.gpu { "DXVA hardware" } else { "software" };
        format!("Media Foundation {} ({kind})", self.codec)
    }

    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<()> {
        // SAFETY: a memory buffer of exactly `len` bytes, filled while locked, wrapped in a
        // sample and handed to the decoder (which takes its own reference).
        let sample = unsafe {
            let len = access_unit.len() as u32;
            let buffer = MFCreateMemoryBuffer(len)?;
            let mut base = null_mut();
            buffer.Lock(&mut base, None, None)?;
            std::ptr::copy_nonoverlapping(access_unit.as_ptr(), base, access_unit.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            // 100 ns units. Carries the host's timestamp through to the decoded picture.
            sample.SetSampleTime(pts_us as i64 * 10)?;
            sample
        };
        // SAFETY: feeding our transform; NOTACCEPTING means drain first, then retry once.
        unsafe {
            if let Err(e) = self.transform.ProcessInput(0, &sample, 0) {
                if e.code() != MF_E_NOTACCEPTING {
                    return Err(e.into());
                }
                self.drain()?;
                self.transform.ProcessInput(0, &sample, 0)?;
            }
        }
        self.drain()
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // SAFETY: ending the stream on our transform, then balancing our MFStartup.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = MFShutdown();
        }
    }
}

/// A D3D11 device on the default GPU, wrapped in the manager MF uses to share it.
unsafe fn d3d11_device_manager(adapter_luid: Option<u64>) -> Result<(IMFDXGIDeviceManager, ID3D11Device)> {
    // SAFETY: standard D3D11 device creation; MF requires multithread protection on it.
    unsafe {
        // On the renderer's GPU when known (shared textures only work within one GPU).
        let adapter: Option<IDXGIAdapter> = match adapter_luid {
            Some(luid) => {
                let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
                let luid = LUID { LowPart: luid as u32, HighPart: (luid >> 32) as i32 };
                Some(factory.EnumAdapterByLuid(luid).context("renderer's GPU not found")?)
            }
            None => None,
        };
        let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("no D3D11 GPU")?;
        let device = device.context("no D3D11 device")?;
        // Returns the previous setting, which we don't need.
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);

        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("no DXGI device manager")?;
        manager.ResetDevice(&device, token)?;
        Ok((manager, device))
    }
}

/// A decoded NV12 frame in a GPU texture shared with the renderer.
pub struct SharedFrame {
    /// Which pool and slot, so the renderer can reuse what it opened before.
    pub ring_id: u64,
    pub slot: usize,
    /// NT handles the renderer opens with ID3D12Device::OpenSharedHandle.
    pub texture: HANDLE,
    pub fence: HANDLE,
    /// The renderer's GPU queue must wait for the fence to reach this before reading.
    pub fence_value: u64,
    /// Set by the renderer if it can't use shared frames; we then copy instead.
    pub renderer_gave_up: Arc<AtomicBool>,
    /// Keeps the handles open while any frame from this pool is still around.
    _handles: Arc<SharedHandles>,
    /// The slot stays reserved until every clone of this is dropped (the renderer holds one
    /// until the GPU has finished drawing the frame).
    pub lease: Arc<SlotLease>,
}

// SAFETY: NT handles are process-wide values, valid on any thread while open (SharedHandles
// keeps them open for as long as a frame refers to them).
unsafe impl Send for SharedFrame {}

pub struct SlotLease(Arc<AtomicBool>);

impl Drop for SlotLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct SharedHandles {
    fence: HANDLE,
    textures: Vec<HANDLE>,
}

// SAFETY: see SharedFrame.
unsafe impl Send for SharedHandles {}
unsafe impl Sync for SharedHandles {}

impl Drop for SharedHandles {
    fn drop(&mut self) {
        // SAFETY: closing handles we created; nothing uses them after the last reference.
        unsafe {
            let _ = CloseHandle(self.fence);
            for &h in &self.textures {
                let _ = CloseHandle(h);
            }
        }
    }
}

/// A few shared NV12 textures the decoder copies into, plus the fence that tells the
/// renderer each copy has finished.
struct SharedRing {
    id: u64,
    size: (u32, u32),
    context: ID3D11DeviceContext,
    context4: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    fence_value: u64,
    textures: Vec<ID3D11Texture2D>,
    busy: Vec<Arc<AtomicBool>>,
    handles: Arc<SharedHandles>,
    renderer_gave_up: Arc<AtomicBool>,
}

/// Displayed + in flight on the GPU + waiting to be drawn, plus a spare.
const RING_SLOTS: usize = 4;

impl SharedRing {
    fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        // SAFETY: creating shareable resources on our own device.
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0) as u32,
            };
            let mut textures = Vec::new();
            let mut texture_handles = Vec::new();
            for _ in 0..RING_SLOTS {
                let mut texture = None;
                device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = texture.context("no shared texture")?;
                let handle = texture.cast::<IDXGIResource1>()?.CreateSharedHandle(
                    None,
                    (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
                    PCWSTR::null(),
                )?;
                textures.push(texture);
                texture_handles.push(handle);
            }
            let mut fence: Option<ID3D11Fence> = None;
            device.cast::<ID3D11Device5>()?.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence)?;
            let fence = fence.context("no shared fence")?;
            let fence_handle = fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())?;
            let context = device.GetImmediateContext()?;
            let context4 = context.cast::<ID3D11DeviceContext4>()?;
            tracing::info!("zero-copy display: sharing {RING_SLOTS} {width}x{height} textures with the renderer");
            Ok(Self {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                size: (width, height),
                context,
                context4,
                fence,
                fence_value: 0,
                textures,
                busy: (0..RING_SLOTS).map(|_| Arc::new(AtomicBool::new(false))).collect(),
                handles: Arc::new(SharedHandles { fence: fence_handle, textures: texture_handles }),
                renderer_gave_up: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    /// A slot the renderer isn't using, reserved for the caller.
    fn free_slot(&self) -> Option<usize> {
        self.busy.iter().position(|b| b.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_ok())
    }
}

/// The visible picture size, if the decoder reports one smaller than the coded size.
unsafe fn display_aperture(t: &IMFMediaType) -> Option<(u32, u32)> {
    let mut area = MFVideoArea::default();
    // SAFETY: MFVideoArea is plain data and the blob is exactly its size.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, size_of::<MFVideoArea>())
    };
    // SAFETY: reading a blob attribute into a correctly sized buffer.
    unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) }.ok()?;
    (area.Area.cx > 0 && area.Area.cy > 0).then_some((area.Area.cx as u32, area.Area.cy as u32))
}
