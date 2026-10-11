//! H.264 / HEVC decoding with Windows Media Foundation's built-in decoders.
//!
//! Given a D3D11 device the decoder uses DXVA, i.e. the GPU's fixed-function video
//! decoder, which keeps CPU use tiny even on low-power chips. Without a usable GPU it
//! still works, decoding in software inside Windows' own (licensed) decoder.
//!
//! Windows "N" editions ship without Media Foundation; installing Microsoft's Media
//! Feature Pack adds it. The HEVC decoder only exists once Microsoft's "HEVC Video
//! Extensions" are installed (often preinstalled by PC makers); without them we use H.264.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use anyhow::{Context, Result, bail};
use fmp_proto::Codec;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
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
    sample_time: i64,
    deliver: Deliver,
    // Kept alive for as long as the decoder uses it.
    _device_manager: Option<IMFDXGIDeviceManager>,
}

impl MfDecoder {
    pub fn new(codec: Codec, deliver: Deliver) -> Result<Self> {
        // SAFETY: per-thread COM init and refcounted MF startup; this thread owns the decoder.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("Media Foundation is missing (Windows N? install the Media Feature Pack)")?;
        }
        match Self::create(codec, deliver.clone(), true) {
            Ok(d) => Ok(d),
            Err(e) => {
                tracing::warn!("GPU decoding unavailable ({e:#}); using Windows' software decoder");
                Self::create(codec, deliver, false)
            }
        }
    }

    fn create(codec: Codec, deliver: Deliver, gpu: bool) -> Result<Self> {
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

            let device_manager = if gpu {
                if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                    bail!("decoder isn't D3D11 aware");
                }
                let manager = d3d11_device_manager()?;
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
                Some(manager)
            } else {
                None
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
                sample_time: 0,
                deliver,
                _device_manager: device_manager,
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
                        if let Some(sample) = sample
                            && let Some(picture) = self.read_nv12(&sample)?
                        {
                            (self.deliver)(picture);
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output_type()?,
                    Err(e) => return Err(e.into()),
                }
            }
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
            let copy = |base: *const u8, pitch: usize| {
                let y = std::slice::from_raw_parts(base, pitch * h as usize).to_vec();
                let uv = std::slice::from_raw_parts(base.add(pitch * coded_h), pitch * h.div_ceil(2) as usize).to_vec();
                YuvFrame { width: w, height: h, planes: Planes::Nv12 { y, uv, y_stride: pitch as u32, uv_stride: pitch as u32 } }
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

    fn decode(&mut self, access_unit: &[u8]) -> Result<()> {
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
            // 100 ns units; only needs to increase.
            sample.SetSampleTime(self.sample_time)?;
            self.sample_time += 166_667;
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
unsafe fn d3d11_device_manager() -> Result<IMFDXGIDeviceManager> {
    // SAFETY: standard D3D11 device creation; MF requires multithread protection on it.
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
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
        Ok(manager)
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
