//! Hardware H.264 / HEVC encoding with the GPU's Media Foundation encoder (Intel Quick Sync,
//! NVIDIA NVENC or AMD AMF, whichever Windows offers), fed NV12 textures straight from
//! capture on the shared device.
//!
//! Hardware encoder MFTs are asynchronous: they ask for input (METransformNeedInput) and
//! announce output (METransformHaveOutput) through events. In low-latency mode each frame
//! in gives one frame out, so `encode` hands one frame over and waits briefly for it.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::Codec;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4};
use windows::Win32::Foundation::VARIANT_BOOL;
use windows::core::{Interface, PWSTR};

use super::d3d;
use crate::encoder::{Encoded, VideoEncoder};
use crate::frame::{Frame, I420Frame};

/// MF_VERSION from mfapi.h: (MF_SDK_VERSION << 16) | MF_API_VERSION.
const MF_VERSION: u32 = (0x0002 << 16) | 0x0070;
/// How long to wait for the encoder to take a frame or give one back.
const WAIT: Duration = Duration::from_millis(250);
// eAVEncH264VProfile_High and eAVEncH265VProfile_Main_420_8.
const H264_PROFILE_HIGH: u32 = 100;
const HEVC_PROFILE_MAIN: u32 = 1;

pub struct MfEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    codec: Codec,
    name: &'static str,
    fps: u32,
    size: (u32, u32),
    frame_index: u64,
    /// Inputs the encoder has asked for and not been given yet.
    wanted: u32,
    /// Outputs it has announced and we haven't collected.
    ready: u32,
    /// SPS/PPS (and VPS for HEVC), Annex-B, for keyframes that arrive without them.
    parameter_sets: Vec<u8>,
    /// For test-pattern frames, which arrive in system memory.
    upload: Option<ID3D11Texture2D>,
}

// SAFETY: Media Foundation objects created in the multithreaded apartment; used from one
// thread at a time (the session hands the encoder to its encoder thread).
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    pub fn new(codec: Codec, width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        // SAFETY: Media Foundation setup on valid objects; see each step.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("Media Foundation is missing (Windows N? install the Media Feature Pack)")?;
            let (transform, name) = find_hardware_encoder(codec)?;

            let attributes = transform.GetAttributes()?;
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).context("can't unlock the asynchronous encoder")?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);

            // Encode straight from capture's textures.
            let d = d3d::shared()?;
            let (mut token, mut manager) = (0u32, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager: IMFDXGIDeviceManager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&d.device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize).context("the encoder can't use our GPU device")?;

            // Output first (encoders want that order), then input.
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &subtype(codec))?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            output.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
            output.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
            output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
            output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            let profile = if codec == Codec::H264 { H264_PROFILE_HIGH } else { HEVC_PROFILE_MAIN };
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
            // Signal BT.709 limited range in the stream, matching what capture produces.
            let _ = output.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = output.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            let _ = output.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = output.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            transform.SetOutputType(0, &output, 0).context("the encoder rejected the output format")?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetInputType(0, &input, 0).context("the encoder rejected NV12 input")?;

            // Low latency, steady bitrate, and an IDR now and then so a client recovers on
            // its own even if a keyframe request is lost. All optional: encoders differ.
            let codec_api: Option<ICodecAPI> = transform.cast().ok();
            if let Some(api) = &codec_api {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &variant_bool(true));
                if api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_LowDelayVBR.0 as u32)).is_err() {
                    let _ = api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
                }
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
                let _ = api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(fps * 10));
            }

            let parameter_sets = sequence_header(&transform).unwrap_or_default();
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let events: IMFMediaEventGenerator = transform.cast()?;
            tracing::info!("hardware encoder: {name}");
            Ok(Self {
                transform,
                events,
                codec_api,
                codec,
                name: Box::leak(format!("Media Foundation {codec} ({name})").into_boxed_str()),
                fps: fps.max(1),
                size: (width, height),
                frame_index: 0,
                wanted: 0,
                ready: 0,
                parameter_sets,
                upload: None,
            })
        }
    }

    /// Collects pending events, waiting until `done` holds or `WAIT` runs out.
    fn pump(&mut self, done: impl Fn(&Self) -> bool) -> Result<bool> {
        let deadline = Instant::now() + WAIT;
        loop {
            if done(self) {
                return Ok(true);
            }
            // SAFETY: polling our encoder's event queue.
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    // SAFETY: reading the event's type.
                    let kind = MF_EVENT_TYPE(unsafe { event.GetType()? } as i32);
                    if kind == METransformNeedInput {
                        self.wanted += 1;
                    } else if kind == METransformHaveOutput {
                        self.ready += 1;
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_micros(500));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// A texture for a frame: capture's own, or test-pattern pixels uploaded to one.
    fn texture_for(&mut self, frame: &Frame) -> Result<ID3D11Texture2D> {
        match frame {
            Frame::Native(f) => Ok(f.texture.clone()),
            Frame::I420(f) => {
                if self.upload.is_none() {
                    self.upload = Some(nv12_texture(self.size)?);
                }
                let texture = self.upload.clone().unwrap();
                upload_i420(&texture, f)?;
                Ok(texture)
            }
        }
    }

    fn take_output(&mut self) -> Result<Option<Encoded>> {
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, ..Default::default() }];
        let mut status = 0u32;
        // SAFETY: the encoder provides the output sample (hardware MFTs allocate their own).
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        let [buffer] = buffers;
        drop(ManuallyDrop::into_inner(buffer.pEvents));
        let sample = ManuallyDrop::into_inner(buffer.pSample);
        match result {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // The encoder changed its output format; accept the one it offers.
                // SAFETY: renegotiating the output type the encoder proposes.
                unsafe {
                    let offered = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &offered, 0)?;
                }
                self.parameter_sets = sequence_header(&self.transform).unwrap_or_default();
                return Ok(None);
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let sample = sample.context("the encoder returned no sample")?;
        // SAFETY: reading the encoded bytes out of the sample's buffer.
        let (mut data, keyframe) = unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            (data, sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0)
        };
        // Clients need parameter sets with every keyframe.
        if keyframe && !has_parameter_sets(self.codec, &data) && !self.parameter_sets.is_empty() {
            data.splice(0..0, self.parameter_sets.iter().copied());
        }
        Ok(Some(Encoded { data, keyframe }))
    }
}

impl VideoEncoder for MfEncoder {
    fn name(&self) -> &'static str {
        self.name
    }

    fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<Encoded>> {
        if !self.pump(|s| s.wanted > 0)? {
            bail!("the encoder stopped taking frames");
        }
        let texture = self.texture_for(frame)?;
        if force_keyframe && let Some(api) = &self.codec_api {
            // SAFETY: a documented per-frame setting.
            let _ = unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)) };
        }
        // SAFETY: wrapping our texture in a sample; the encoder reads it on the same device.
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, 0, false)?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(two_d.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let duration = 10_000_000 / self.fps as i64;
            sample.SetSampleTime(self.frame_index as i64 * duration)?;
            sample.SetSampleDuration(duration)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.frame_index += 1;
        self.wanted -= 1;
        // Low-latency encoders return each frame right away.
        if !self.pump(|s| s.ready > 0)? {
            return Ok(None);
        }
        self.ready -= 1;
        self.take_output()
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<()> {
        let Some(api) = &self.codec_api else { bail!("this encoder can't change its bitrate") };
        // SAFETY: a documented setting, allowed mid-stream.
        unsafe { api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bps))? };
        Ok(())
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        // SAFETY: ending the stream on our transform, then balancing our MFStartup.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            let _ = MFShutdown();
        }
    }
}

fn subtype(codec: Codec) -> windows::core::GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::H265 => MFVideoFormat_HEVC,
    }
}

/// The first hardware encoder Windows lists for NV12 in, `codec` out, and its name.
unsafe fn find_hardware_encoder(codec: Codec) -> Result<(IMFTransform, String)> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
    // SAFETY: enumeration with valid type infos; we take ownership of the returned array.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut count,
        )?;
        let activates: Vec<Option<IMFActivate>> = (0..count as usize).map(|i| list.add(i).read()).collect();
        CoTaskMemFree(Some(list as _));
        let activate = activates.into_iter().flatten().next().with_context(|| format!("no hardware {codec} encoder on this PC"))?;
        let mut name = PWSTR::null();
        let mut len = 0u32;
        let friendly = match activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len) {
            Ok(()) => {
                let s = name.to_string().unwrap_or_default();
                CoTaskMemFree(Some(name.0 as _));
                s
            }
            Err(_) => "hardware".into(),
        };
        let transform: IMFTransform = activate.ActivateObject().context("couldn't start the hardware encoder")?;
        Ok((transform, friendly))
    }
}

/// The encoder's parameter sets (Annex-B), if it reports them.
fn sequence_header(transform: &IMFTransform) -> Option<Vec<u8>> {
    // SAFETY: reading a blob attribute of the current output type into a buffer of its size.
    unsafe {
        let current = transform.GetOutputCurrentType(0).ok()?;
        let size = current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        let mut blob = vec![0u8; size as usize];
        current.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).ok()?;
        Some(blob)
    }
}

/// Whether an Annex-B access unit contains a sequence parameter set.
fn has_parameter_sets(codec: Codec, data: &[u8]) -> bool {
    let sps = codec.parameter_set_types()[codec.parameter_set_types().len() - 2];
    data.windows(4)
        .enumerate()
        .filter(|(_, w)| w[..3] == [0, 0, 1])
        .any(|(i, _)| codec.nal_type(&data[i + 3..]) == sps)
}

fn nv12_texture(size: (u32, u32)) -> Result<ID3D11Texture2D> {
    let d = d3d::shared()?;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    // SAFETY: a valid description and out pointer.
    unsafe { d.device.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.context("no texture")
}

/// Copies an I420 picture into an NV12 texture (test pattern only).
fn upload_i420(texture: &ID3D11Texture2D, f: &I420Frame) -> Result<()> {
    let (w, h) = (f.width, f.height);
    let mut nv12 = Vec::with_capacity(w * h * 3 / 2);
    nv12.extend_from_slice(&f.y);
    for (u, v) in f.u.iter().zip(&f.v) {
        nv12.extend_from_slice(&[*u, *v]);
    }
    let d = d3d::shared()?;
    // SAFETY: NV12 texture updates take the Y rows then the CbCr rows, both `w` bytes apart.
    unsafe { d.context.UpdateSubresource(texture, 0, None, nv12.as_ptr() as _, w as u32, (w * h) as u32) };
    Ok(())
}

fn variant_u32(value: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: value },
            }),
        },
    }
}

fn variant_bool(value: bool) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { boolVal: VARIANT_BOOL(if value { -1 } else { 0 }) },
            }),
        },
    }
}
