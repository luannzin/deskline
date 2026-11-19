//! The GPU's H.264 encoder through Media Foundation: NVENC, Quick Sync and AMF all register as
//! hardware encoder MFTs. Hardware MFTs are always asynchronous: they ask for input and announce
//! output with events, which we poll so a stuck driver can't hang the stream.
//!
//! Given the capture's GPU, the encoder works there end to end: the video processor scales each
//! captured texture and converts it to NV12, and the encoder reads that texture. No pixel goes
//! through memory. Otherwise it takes NV12 pictures from memory.
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Context;
use openh264::formats::{YUVBuffer, YUVSource};
use openh264::nal_units;
use windows::Win32::Foundation::{LUID, S_OK};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HRESULT, Interface, PWSTR};

use super::{Headers, nal_kind};

/// A hardware encoder answers in a few ms; this long means it's stuck or holding frames back.
const PATIENCE: Duration = Duration::from_millis(500);

pub struct Encoder {
    activate: IMFActivate,
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    /// Input and output stream ids.
    ids: (u32, u32),
    w: usize,
    h: usize,
    /// Frames a second it was set up for: its rate control spreads the bitrate over them.
    fps: u32,
    frames: i64,
    /// Inputs the encoder asked for and hasn't been given yet.
    wants: u32,
    headers: Headers,
    pub name: String,
    /// Pictures come as textures on this GPU (`encode_gpu`) rather than from memory.
    gpu: Option<Convert>,
}

// Everything lives in the multithreaded apartment (`com()` runs first on every thread that
// touches it), so any thread may use it, and `&mut self` keeps it to one at a time.
unsafe impl Send for Encoder {}

/// Join the multithreaded apartment, once per thread: tokio may run us on any worker.
fn com() {
    thread_local!(static JOINED: () = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    });
    JOINED.with(|_| {});
}

fn startup() -> anyhow::Result<()> {
    static UP: OnceLock<HRESULT> = OnceLock::new();
    UP.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_or_else(|e| e.code(), |_| S_OK)).ok()?;
    Ok(())
}

impl Encoder {
    /// The first hardware encoder that takes NV12 at `w`x`h` and `fps` and gives Constrained
    /// Baseline H.264 at a constant `bps`, with no frames held back. With `gpu`, only encoders
    /// on that GPU that can read its textures.
    pub fn new(w: usize, h: usize, bps: u32, fps: u32, gpu: Option<&ID3D11Device>) -> anyhow::Result<Self> {
        com();
        startup()?;
        let video = |subtype| MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
        let (input, output) = (video(MFVideoFormat_NV12), video(MFVideoFormat_H264));
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let (mut list, mut n) = (std::ptr::null_mut(), 0);
        let mut found: Vec<IMFActivate> = unsafe {
            MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(&raw const input), Some(&raw const output), &mut list, &mut n)?;
            if list.is_null() {
                Vec::new()
            } else {
                let found = std::slice::from_raw_parts_mut(list, n as usize).iter_mut().filter_map(Option::take).collect();
                CoTaskMemFree(Some(list as *const _));
                found
            }
        };
        if let Some(device) = gpu {
            let luid = unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?.AdapterLuid };
            found.retain(|a| on_adapter(a, luid));
        }
        anyhow::ensure!(!found.is_empty(), "no hardware H.264 encoder on this computer{}", if gpu.is_some() { "'s capturing GPU" } else { "" });
        let mut why = Vec::new();
        for a in found {
            let name = name(&a);
            match Self::open(a, name.clone(), w, h, bps, fps, gpu) {
                Ok(e) => return Ok(e),
                Err(e) => why.push(format!("{name}: {e:#}")),
            }
        }
        anyhow::bail!("{}", why.join("; "))
    }

    fn open(activate: IMFActivate, name: String, w: usize, h: usize, bps: u32, fps: u32, gpu: Option<&ID3D11Device>) -> anyhow::Result<Self> {
        let mft: IMFTransform = unsafe { activate.ActivateObject()? };
        let (Ok(events), Ok(codec)) = (mft.cast::<IMFMediaEventGenerator>(), mft.cast::<ICodecAPI>()) else {
            unsafe { activate.ShutdownObject()? };
            anyhow::bail!("no event generator or ICodecAPI");
        };
        let mut enc = Self {
            activate,
            mft,
            events,
            codec,
            ids: (0, 0),
            w,
            h,
            fps,
            frames: 0,
            wants: 0,
            headers: Headers::default(),
            name,
            gpu: None,
        };
        // On failure the drop shuts it down.
        enc.configure(bps, gpu)?;
        Ok(enc)
    }

    fn configure(&mut self, bps: u32, gpu: Option<&ID3D11Device>) -> anyhow::Result<()> {
        let (w, h, fps) = (self.w as u64, self.h as u64, u64::from(self.fps));
        unsafe {
            let attrs = self.mft.GetAttributes()?;
            anyhow::ensure!(attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1, "not asynchronous");
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            // The GPU comes before the media types: they're checked against it.
            if let Some(device) = gpu {
                self.gpu = Some(Convert::attach(&self.mft, &attrs, device, self.w as u32, self.h as u32)?);
            }
            let (mut i, mut o) = ([0u32], [0u32]);
            // E_NOTIMPL means the ids are simply 0.
            if self.mft.GetStreamIDs(&mut i, &mut o).is_ok() {
                self.ids = (i[0], o[0]);
            }

            let video = |subtype: &GUID| -> windows::core::Result<IMFMediaType> {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, (w << 32) | h)?;
                t.SetUINT64(&MF_MT_FRAME_RATE, (fps << 32) | 1)?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                Ok(t)
            };
            // Encoders take the output type first.
            let out = video(&MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bps)?;
            let profile = |p: eAVEncH264VProfile| {
                out.SetUINT32(&MF_MT_MPEG2_PROFILE, p.0 as u32)?;
                self.mft.SetOutputType(self.ids.1, &out, 0)
            };
            profile(eAVEncH264VProfile_ConstrainedBase).or_else(|_| profile(eAVEncH264VProfile_Base)).context("H.264 baseline output")?;
            let input = video(&MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w as u32)?;
            self.mft.SetInputType(self.ids.0, &input, 0).context("NV12 input")?;

            self.set(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32).context("CBR")?;
            self.set(&CODECAPI_AVEncCommonMeanBitRate, bps).context("bitrate")?;
            // Nice to have: one frame out per frame in is checked on every frame anyway.
            let _ = self.codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            // Keyframes only when the viewer asks (or after two minutes).
            let _ = self.set(&CODECAPI_AVEncMPVGOPSize, 3600);
            let _ = self.set(&CODECAPI_AVEncMPVDefaultBPictureCount, 0);

            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(())
    }

    fn set(&self, api: &GUID, value: u32) -> windows::core::Result<()> {
        unsafe { self.codec.SetValue(api, &VARIANT::from(value)) }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Change the bitrate without restarting. False when the encoder won't.
    pub fn set_bitrate(&mut self, bps: u32) -> bool {
        com();
        self.set(&CODECAPI_AVEncCommonMeanBitRate, bps).is_ok()
    }

    /// One picture from memory in, its H.264 out. Errors if the encoder holds the picture back.
    pub fn encode(&mut self, yuv: &YUVBuffer, key: bool) -> anyhow::Result<Vec<u8>> {
        com();
        let sample = self.sample(yuv)?;
        self.push(sample, key)
    }

    /// One BGRA texture from the capturing GPU in, any size: scaled and converted to NV12 there.
    pub fn encode_gpu(&mut self, picture: &ID3D11Texture2D, key: bool) -> anyhow::Result<Vec<u8>> {
        com();
        let size = (self.w as u32, self.h as u32);
        let gpu = self.gpu.as_mut().context("this encoder takes pictures from memory")?;
        gpu.convert(picture, size)?;
        let sample = gpu.sample()?;
        self.push(sample, key)
    }

    fn push(&mut self, sample: IMFSample, key: bool) -> anyhow::Result<Vec<u8>> {
        let deadline = Instant::now() + PATIENCE;
        let mut out = Vec::new();
        while self.wants == 0 {
            self.event(deadline, &mut out)?;
        }
        if key {
            self.set(&CODECAPI_AVEncVideoForceKeyFrame, 1).context("forcing a keyframe")?;
        }
        // One frame in Media Foundation's 100 ns units.
        let frame = 10_000_000 / i64::from(self.fps);
        unsafe {
            sample.SetSampleTime(self.frames * frame)?;
            sample.SetSampleDuration(frame)?;
            self.mft.ProcessInput(self.ids.0, &sample, 0)?;
        }
        self.wants -= 1;
        self.frames += 1;
        // Some encoders send the SPS and PPS as a sample of their own: wait for the picture.
        while !nal_units(&out).any(|n| matches!(nal_kind(n), Some(1 | 5))) {
            self.event(deadline, &mut out)?;
        }
        self.headers.fix(&mut out)?;
        Ok(out)
    }

    /// Wait for the encoder's next event and act on it.
    fn event(&mut self, deadline: Instant, out: &mut Vec<u8>) -> anyhow::Result<()> {
        let e = loop {
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(e) => break e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    anyhow::ensure!(Instant::now() < deadline, "no answer in {} ms", PATIENCE.as_millis());
                    std::thread::sleep(Duration::from_micros(500));
                }
                Err(e) => return Err(e.into()),
            }
        };
        unsafe {
            e.GetStatus()?.ok()?;
            match e.GetType()? as i32 {
                t if t == METransformNeedInput.0 => self.wants += 1,
                t if t == METransformHaveOutput.0 => self.output(out)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn output(&mut self, out: &mut Vec<u8>) -> anyhow::Result<()> {
        unsafe {
            let info = self.mft.GetOutputStreamInfo(self.ids.1)?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            let sample = if info.dwFlags & provides != 0 {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max((self.w * self.h) as u32))?)?;
                Some(s)
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.ids.1,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
            let sample = ManuallyDrop::take(&mut buf[0].pSample);
            drop(ManuallyDrop::take(&mut buf[0].pEvents));
            match r {
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // The encoder settled its output type: take it and collect the output.
                    let t = self.mft.GetOutputAvailableType(self.ids.1, 0)?;
                    self.mft.SetOutputType(self.ids.1, &t, 0)?;
                    return self.output(out);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                r => r?,
            }
            let buffer = sample.context("no output sample")?.ConvertToContiguousBuffer()?;
            let (mut p, mut n) = (std::ptr::null_mut(), 0);
            buffer.Lock(&mut p, None, Some(&raw mut n))?;
            out.extend_from_slice(std::slice::from_raw_parts(p, n as usize));
            buffer.Unlock()?;
        }
        Ok(())
    }

    /// The I420 picture as an NV12 sample: same Y plane, U and V interleaved.
    fn sample(&self, yuv: &YUVBuffer) -> anyhow::Result<IMFSample> {
        let (w, h) = (self.w, self.h);
        anyhow::ensure!(yuv.dimensions() == (w, h), "picture is {:?}, encoder {w}x{h}", yuv.dimensions());
        let len = w * h * 3 / 2;
        unsafe {
            let buf = MFCreateMemoryBuffer(len as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            let (y, uv) = std::slice::from_raw_parts_mut(p, len).split_at_mut(w * h);
            y.copy_from_slice(yuv.y());
            for ((d, &u), &v) in uv.chunks_exact_mut(2).zip(yuv.u()).zip(yuv.v()) {
                d[0] = u;
                d[1] = v;
            }
            buf.Unlock()?;
            buf.SetCurrentLength(len as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            Ok(s)
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        com();
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// The GPU half of an encoder that reads the capture's textures: the video processor scales each
/// one and converts it to NV12 in a texture the encoder reads.
struct Convert {
    video: ID3D11VideoDevice,
    ctx: ID3D11VideoContext,
    /// Hands the device to the encoder; kept for as long as it encodes.
    _manager: IMFDXGIDeviceManager,
    nv12: ID3D11Texture2D,
    /// Made for one picture size; another size makes it again.
    processor: Option<Processor>,
}

struct Processor {
    from: (u32, u32),
    list: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    out: ID3D11VideoProcessorOutputView,
}

impl Convert {
    /// Share `device` with the encoder, which must be able to read its textures, and make the
    /// `w`x`h` NV12 texture it reads.
    unsafe fn attach(mft: &IMFTransform, attrs: &IMFAttributes, device: &ID3D11Device, w: u32, h: u32) -> anyhow::Result<Self> {
        unsafe {
            anyhow::ensure!(attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 1, "can't read textures (not D3D11-aware)");
            let (mut token, mut manager) = (0, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut nv12 = None;
            device.CreateTexture2D(&desc, None, Some(&mut nv12))?;
            Ok(Self {
                video: device.cast()?,
                ctx: device.GetImmediateContext()?.cast()?,
                _manager: manager,
                nv12: nv12.context("no NV12 texture")?,
                processor: None,
            })
        }
    }

    /// `picture` (BGRA, any size) scaled into the NV12 texture.
    fn convert(&mut self, picture: &ID3D11Texture2D, to: (u32, u32)) -> anyhow::Result<()> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            picture.GetDesc(&mut desc);
            let from = (desc.Width, desc.Height);
            if self.processor.as_ref().is_none_or(|p| p.from != from) {
                self.processor = Some(Processor::new(&self.video, &self.ctx, &self.nv12, from, to)?);
            }
            let p = self.processor.as_ref().expect("just made");
            let view = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut input = None;
            self.video.CreateVideoProcessorInputView(picture, &p.list, &view, Some(&mut input))?;
            let mut streams =
                [D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(input), ..Default::default() }];
            let done = self.ctx.VideoProcessorBlt(&p.vp, &p.out, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            Ok(done?)
        }
    }

    /// The NV12 texture as the encoder's next input.
    fn sample(&self) -> anyhow::Result<IMFSample> {
        unsafe {
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &self.nv12, 0, false)?;
            buf.SetCurrentLength(buf.cast::<IMF2DBuffer>()?.GetContiguousLength()?)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            Ok(s)
        }
    }
}

impl Processor {
    unsafe fn new(video: &ID3D11VideoDevice, ctx: &ID3D11VideoContext, nv12: &ID3D11Texture2D, from: (u32, u32), to: (u32, u32)) -> anyhow::Result<Self> {
        unsafe {
            let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: from.0,
                InputHeight: from.1,
                OutputFrameRate: rate,
                OutputWidth: to.0,
                OutputHeight: to.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let list = video.CreateVideoProcessorEnumerator(&content)?;
            let can = |f, what: D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT| list.CheckVideoProcessorFormat(f).is_ok_and(|s| s & what.0 as u32 != 0);
            anyhow::ensure!(
                can(DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT)
                    && can(DXGI_FORMAT_NV12, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT),
                "the video processor can't turn BGRA into NV12"
            );
            let vp = video.CreateVideoProcessor(&list, 0)?;
            let view = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out = None;
            video.CreateVideoProcessorOutputView(nv12, &list, &view, Some(&mut out))?;
            // Full-range RGB in, BT.601 limited-range YCbCr out: what OpenH264 encodes and every
            // viewer decodes, so colors match whichever encoder made the picture.
            let ctx1 = ctx.cast::<ID3D11VideoContext1>()?;
            ctx1.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            ctx1.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601);
            ctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // Desktop text, not film: none of the driver's denoising or sharpening.
            ctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            Ok(Self { from, list, vp, out: out.context("no output view")? })
        }
    }
}

/// Whether `a` runs on the GPU with this LUID. Encoders that don't say are tried; the device
/// manager refuses one that can't use the device.
fn on_adapter(a: &IMFActivate, luid: LUID) -> bool {
    let mut blob = [0u8; 8];
    match unsafe { a.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut blob, None) } {
        Ok(()) => blob[..4] == luid.LowPart.to_le_bytes() && blob[4..] == luid.HighPart.to_le_bytes(),
        Err(_) => true,
    }
}

fn name(a: &IMFActivate) -> String {
    let (mut s, mut n) = (PWSTR::null(), 0);
    unsafe {
        if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut s, &mut n).is_err() {
            return "hardware encoder".into();
        }
        let name = s.to_string().unwrap_or_default();
        CoTaskMemFree(Some(s.0 as *const _));
        name
    }
}
