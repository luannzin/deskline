//! The GPU's H.264 encoder through Media Foundation: NVENC, Quick Sync and AMF all register as
//! hardware encoder MFTs. Hardware MFTs are always asynchronous: they ask for input and announce
//! output with events, which we poll so a stuck driver can't hang the stream.
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Context;
use openh264::formats::{YUVBuffer, YUVSource};
use openh264::nal_units;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HRESULT, Interface, PWSTR};

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
    /// The stream's SPS and PPS, put back in front of keyframes that come without them.
    headers: Vec<u8>,
    pub name: String,
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
    /// Baseline H.264 at a constant `bps`, with no frames held back.
    pub fn new(w: usize, h: usize, bps: u32, fps: u32) -> anyhow::Result<Self> {
        com();
        startup()?;
        let video = |subtype| MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
        let (input, output) = (video(MFVideoFormat_NV12), video(MFVideoFormat_H264));
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let (mut list, mut n) = (std::ptr::null_mut(), 0);
        let found: Vec<IMFActivate> = unsafe {
            MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(&raw const input), Some(&raw const output), &mut list, &mut n)?;
            if list.is_null() {
                Vec::new()
            } else {
                let found = std::slice::from_raw_parts_mut(list, n as usize).iter_mut().filter_map(Option::take).collect();
                CoTaskMemFree(Some(list as *const _));
                found
            }
        };
        anyhow::ensure!(!found.is_empty(), "no hardware H.264 encoder on this computer");
        let mut why = Vec::new();
        for a in found {
            let name = name(&a);
            match Self::open(a, name.clone(), w, h, bps, fps) {
                Ok(e) => return Ok(e),
                Err(e) => why.push(format!("{name}: {e:#}")),
            }
        }
        anyhow::bail!("{}", why.join("; "))
    }

    fn open(activate: IMFActivate, name: String, w: usize, h: usize, bps: u32, fps: u32) -> anyhow::Result<Self> {
        let mft: IMFTransform = unsafe { activate.ActivateObject()? };
        let (Ok(events), Ok(codec)) = (mft.cast::<IMFMediaEventGenerator>(), mft.cast::<ICodecAPI>()) else {
            unsafe { activate.ShutdownObject()? };
            anyhow::bail!("no event generator or ICodecAPI");
        };
        let mut enc =
            Self { activate, mft, events, codec, ids: (0, 0), w, h, fps, frames: 0, wants: 0, headers: Vec::new(), name };
        // On failure the drop shuts it down.
        enc.configure(bps)?;
        Ok(enc)
    }

    fn configure(&mut self, bps: u32) -> anyhow::Result<()> {
        let (w, h, fps) = (self.w as u64, self.h as u64, u64::from(self.fps));
        unsafe {
            let attrs = self.mft.GetAttributes()?;
            anyhow::ensure!(attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1, "not asynchronous");
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
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

    /// One picture in, its H.264 out. Errors if the encoder holds the picture back.
    pub fn encode(&mut self, yuv: &YUVBuffer, key: bool) -> anyhow::Result<Vec<u8>> {
        com();
        let deadline = Instant::now() + PATIENCE;
        let mut out = Vec::new();
        while self.wants == 0 {
            self.event(deadline, &mut out)?;
        }
        if key {
            self.set(&CODECAPI_AVEncVideoForceKeyFrame, 1).context("forcing a keyframe")?;
        }
        let sample = self.sample(yuv)?;
        unsafe { self.mft.ProcessInput(self.ids.0, &sample, 0)? };
        self.wants -= 1;
        self.frames += 1;
        // Some encoders send the SPS and PPS as a sample of their own: wait for the picture.
        while !nal_units(&out).any(|n| matches!(kind(n), Some(1 | 5))) {
            self.event(deadline, &mut out)?;
        }
        self.headers(&mut out)?;
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
            // One frame in Media Foundation's 100 ns units.
            let frame = 10_000_000 / i64::from(self.fps);
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(self.frames * frame)?;
            s.SetSampleDuration(frame)?;
            Ok(s)
        }
    }

    /// Check the profile on the first SPS, keep SPS and PPS, and put them in front of any
    /// keyframe that lacks them so a viewer can always start from a keyframe.
    fn headers(&mut self, out: &mut Vec<u8>) -> anyhow::Result<()> {
        let (mut sps, mut idr) = (false, false);
        let mut found = Vec::new();
        for n in nal_units(out) {
            match kind(n) {
                Some(7) => {
                    sps = true;
                    let profile = n.iter().skip_while(|&&b| b != 1).nth(2).copied();
                    anyhow::ensure!(profile == Some(66), "it makes profile {profile:?}, not Constrained Baseline (66)");
                    found.extend_from_slice(n);
                }
                Some(8) => found.extend_from_slice(n),
                Some(5) => idr = true,
                _ => {}
            }
        }
        if sps {
            self.headers = found;
        } else if idr {
            anyhow::ensure!(!self.headers.is_empty(), "keyframe without SPS");
            out.splice(0..0, self.headers.iter().copied());
        }
        Ok(())
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

/// NAL unit type of one Annex B unit (start code included).
fn kind(nal: &[u8]) -> Option<u8> {
    nal.iter().skip_while(|&&b| b != 1).nth(1).map(|b| b & 0x1f)
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
