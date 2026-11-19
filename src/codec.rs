//! H.264 both ways. The encoder sizes the picture from the quality preset, the link (`set_rate`)
//! and how fast this computer encodes, and uses the GPU's encoder on Windows when there is one;
//! when it's on the GPU that captured the frame, the picture never leaves the GPU.
//! Output is always Constrained Baseline in Annex B, which OpenH264's decoder (the viewer, on
//! every OS) plays.
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::decoder::Decoder as H264Decoder;
use openh264::encoder::{BitRate, Complexity, Encoder as H264Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, RgbaSliceU8, YUVBuffer, YUVSource};
use openh264_sys2::{ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_0, SPATIAL_LAYER_ALL};
use serde::{Deserialize, Serialize};

use crate::desktop::{DEFAULT_FPS, Frame};

#[cfg(windows)]
mod mf;
#[cfg(target_os = "linux")]
mod gst;
/// The GPU's encoder: Media Foundation on Windows, GStreamer on Linux.
#[cfg(target_os = "linux")]
use gst as hw;
#[cfg(windows)]
use mf as hw;

/// OpenH264's largest picture (level 5.2) either way round; the viewer's decoder stops there too.
const MAX_LONG: u64 = 3840;
const MAX_SHORT: u64 = 2160;
/// One step down is 2/3 of the width and height, a bit under half the pixels.
const STEP: (u64, u64) = (2, 3);
const STEP_PIXELS: f64 = 2.25;
/// No step goes below this long side: smaller is too blurry to work with.
const MIN_LONG: u64 = 640;
/// Average encode time, as a share of the time one frame has, above which this computer can't
/// hold the frame rate at a size (28 ms at 30 fps).
const SLOW: f64 = 0.85;
/// Step back up only when the bigger size should take less than this share (18 ms at 30 fps).
const ROOM: f64 = 0.55;
/// H.264 level 5.2's macroblocks a second, the most OpenH264 encodes: 4K stops at 64 fps.
const MAX_MBPS: u64 = 2_073_600;
/// Rates at or below `NET_DOWN` of the top bitrate step the picture down; it steps back up at
/// `NET_UP`. The gap keeps it from flipping.
const NET_DOWN: f32 = 0.35;
const NET_UP: f32 = 0.5;

/// What the viewer prefers when the link or the CPU can't give everything.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Full size up to 1920 wide, a solid bitrate.
    #[default]
    Balanced,
    /// Full size at any width, more bits: crisp text, more bandwidth.
    Sharp,
    /// Smaller picture, fewer bits: for slow links.
    Smooth,
    /// Up to 4K at up to 120 fps, many bits: games and video. Keeps the frame rate and gives
    /// up size first. New in 0.5: added last, so older versions skip it.
    Gaming,
}

impl Quality {
    /// The longest side before any step down, and the top bitrate in bits per pixel per second.
    fn preset(self) -> (u64, f64) {
        match self {
            Quality::Balanced => (1920, 2.0),
            Quality::Sharp => (MAX_LONG, 3.0),
            Quality::Smooth => (1280, 1.0),
            Quality::Gaming => (MAX_LONG, 6.0),
        }
    }

    /// The most frames a second it sends.
    pub fn fps(self) -> u32 {
        if self == Quality::Gaming { 120 } else { DEFAULT_FPS }
    }

    /// No step goes below this long side. A game at 480p isn't playable at any frame rate.
    fn min_long(self) -> u64 {
        if self == Quality::Gaming { 1280 } else { MIN_LONG }
    }
}

pub struct Encoder {
    quality: Quality,
    /// `set_rate`'s fraction of the top bitrate.
    rate: f32,
    /// Steps down for a congested link (0 or 1) and for a slow CPU.
    net_step: bool,
    cpu_steps: u32,
    stream: Option<Stream>,
    /// The hardware encoder failed once: OpenH264 for the rest of the session.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    no_hw: bool,
    /// Reading pictures on the capturing GPU failed once: from memory for the rest of it.
    #[cfg_attr(not(windows), allow(dead_code))]
    no_gpu: bool,
    pace: Pace,
    scaler: Option<Scaler>,
    scratch: Vec<u8>,
    /// The last picture's planes, reused while the size holds.
    yuv: Option<YUVBuffer>,
}

/// The running encoder and what it was opened with.
struct Stream {
    enc: Backend,
    w: usize,
    h: usize,
    bps: u32,
    fps: u32,
    /// The capturing GPU it reads pictures on (`device_id`), 0 when they come from memory.
    gpu: usize,
}

enum Backend {
    Soft(H264Encoder),
    /// With a decoder until its first output has been checked.
    #[cfg(any(windows, target_os = "linux"))]
    Hard(hw::Encoder, Option<Decoder>),
}

/// A picture as an encoder takes it.
enum Picture<'a> {
    Yuv(&'a YUVBuffer),
    /// The captured texture, on the GPU the encoder reads.
    #[cfg(windows)]
    Gpu(&'a windows::Win32::Graphics::Direct3D11::ID3D11Texture2D),
    /// The frame as captured: the GPU scales and converts it.
    #[cfg(target_os = "linux")]
    Frame(&'a Arc<Frame>),
}

/// A GPU pictures are captured on (Windows only).
#[cfg(windows)]
type Device = windows::Win32::Graphics::Direct3D11::ID3D11Device;
#[cfg(not(windows))]
type Device = std::convert::Infallible;

/// Tells GPUs apart: two frames with the same id are on the same device.
#[cfg(windows)]
fn device_id(d: &Device) -> usize {
    windows::core::Interface::as_raw(d) as usize
}
#[cfg(not(windows))]
fn device_id(d: &Device) -> usize {
    match *d {}
}

/// Encode times over the current window of about a second.
struct Pace {
    since: Instant,
    frames: u32,
    busy: Duration,
    /// Windows in a row with room to step up, and how many it takes. That grows with every
    /// step down, so a borderline CPU settles instead of flip-flopping.
    calm: u32,
    up_after: u32,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new(Quality::default())
    }
}

impl Encoder {
    pub fn new(q: Quality) -> Self {
        Self {
            quality: q,
            rate: 1.0,
            net_step: false,
            cpu_steps: 0,
            stream: None,
            no_hw: false,
            no_gpu: false,
            pace: Pace { since: Instant::now(), frames: 0, busy: Duration::ZERO, calm: 0, up_after: 3 },
            scaler: None,
            scratch: Vec::new(),
            yuv: None,
        }
    }

    /// The most frames a second the quality asks for.
    pub fn fps(&self) -> u32 {
        self.quality.fps()
    }

    /// Takes effect on the next frame. A new size starts a new stream.
    pub fn set_quality(&mut self, q: Quality) {
        if q != self.quality {
            self.quality = q;
            // Measure the new size from scratch.
            self.cpu_steps = 0;
        }
    }

    /// Congestion control: `fraction` (0.1..=1.0) of the quality's top bitrate, from the next
    /// frame on, without a keyframe (a restart only if an encoder refuses a 25%+ change). At
    /// or below 0.35 the picture also steps down a size (keyframe) and comes back at 0.5.
    pub fn set_rate(&mut self, fraction: f32) {
        // max() also turns NaN into 0.1.
        self.rate = fraction.max(0.1).min(1.0);
        if self.rate <= NET_DOWN {
            self.net_step = true;
        } else if self.rate >= NET_UP {
            self.net_step = false;
        }
    }

    /// Whether a hardware encoder is in use.
    pub fn hardware(&self) -> bool {
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(Stream { enc: Backend::Hard(..), .. }) = self.stream {
            return true;
        }
        false
    }

    /// Encode one frame; `key` forces a keyframe. A new size starts a new stream (keyframe
    /// with fresh SPS), which the decoder follows on its own.
    pub fn encode(&mut self, f: &Arc<Frame>, key: bool) -> anyhow::Result<Vec<u8>> {
        let started = Instant::now();
        let (w, h) = fit(f.w, f.h, self.quality, self.steps());
        anyhow::ensure!(w >= 16 && h >= 16, "frame too small: {}x{}", f.w, f.h);
        let bps = self.bitrate(f);
        let fps = self.quality.fps();
        let device = self.device(f);
        let gpu = device.as_ref().map_or(0, device_id);

        let mut restart = match &mut self.stream {
            Some(s) if (s.w, s.h, s.fps, s.gpu) == (w, h, fps, gpu) => {
                let change = (bps as f64 / s.bps as f64 - 1.0).abs();
                if change >= 0.05 && s.enc.set_bitrate(bps, s.bps) {
                    s.bps = bps;
                }
                // Couldn't change it in place: only worth a keyframe when the change is big.
                s.bps != bps && change >= 0.25
            }
            _ => true,
        };
        if restart {
            self.stream = None;
            self.stream = Some(self.open(w, h, bps, fps, device.as_ref())?);
        }
        let mut out = self.encode_with(f, w, h, key);
        // A hardware encoder that fails steps down for the rest of the session: from the GPU to
        // memory, then to OpenH264.
        while let Err(e) = &out
            && self.hardware()
        {
            if self.stream.as_ref().is_some_and(|s| s.gpu != 0) {
                eprintln!("deskline: encoding on the GPU failed, encoding from memory: {e:#}");
                self.no_gpu = true;
            } else {
                eprintln!("deskline: the hardware encoder failed, using OpenH264 from now on: {e:#}");
                self.no_hw = true;
            }
            self.stream = None;
            self.stream = Some(self.open(w, h, bps, fps, None)?);
            out = self.encode_with(f, w, h, true);
            restart = true;
        }
        // Opening an encoder (and checking a hardware one) isn't what a frame costs.
        if !restart {
            self.pace(f, started.elapsed());
        }
        out
    }

    /// `f` at `w`x`h` through the open stream: its texture when the encoder reads the capturing
    /// GPU, the frame as captured for a GPU encoder that scales it itself, else I420 pixels from
    /// memory (read back from the GPU if they're only there).
    fn encode_with(&mut self, f: &Arc<Frame>, w: usize, h: usize, key: bool) -> anyhow::Result<Vec<u8>> {
        let stream = self.stream.as_mut().expect("opened before");
        #[cfg(windows)]
        if let Some(g) = &f.gpu
            && stream.gpu != 0
        {
            return stream.enc.encode(Picture::Gpu(&g.tex), key);
        }
        #[cfg(target_os = "linux")]
        if matches!(stream.enc, Backend::Hard(..)) {
            return stream.enc.encode(Picture::Frame(f), key);
        }
        let px = pixels(f)?;
        let yuv = to_yuv(f, &px, w, h, &mut self.scaler, &mut self.scratch, &mut self.yuv);
        stream.enc.encode(Picture::Yuv(yuv), key)
    }

    /// The GPU `f` was captured on, when an encoder may read it there.
    fn device(&self, f: &Frame) -> Option<Device> {
        #[cfg(windows)]
        if !self.no_hw
            && !self.no_gpu
            && let Some(g) = &f.gpu
        {
            return g.device().ok();
        }
        let _ = f;
        None
    }

    fn steps(&self) -> u32 {
        self.cpu_steps + self.net_step as u32
    }

    /// The preset's top bitrate for this screen times `rate`. The top is for the preset's size
    /// before any step down, so `rate` keeps meaning the same while the size moves.
    fn bitrate(&self, f: &Frame) -> u32 {
        let (w, h) = fit(f.w, f.h, self.quality, 0);
        ((w * h) as f64 * self.quality.preset().1 * self.rate as f64).max(10_000.0) as u32
    }

    /// The best encoder there is: the GPU's reading `gpu`'s textures, the GPU's reading memory,
    /// then OpenH264.
    fn open(&mut self, w: usize, h: usize, bps: u32, fps: u32, gpu: Option<&Device>) -> anyhow::Result<Stream> {
        #[cfg(any(windows, target_os = "linux"))]
        if !self.no_hw {
            let hard = |enc, gpu| -> anyhow::Result<Stream> {
                Ok(Stream { enc: Backend::Hard(enc, Some(Decoder::new()?)), w, h, bps, fps, gpu })
            };
            #[cfg(windows)]
            if let Some(device) = gpu {
                match hw::Encoder::new(w, h, bps, fps, Some(device)) {
                    Ok(enc) => return hard(enc, device_id(device)),
                    Err(e) => {
                        self.no_gpu = true;
                        eprintln!("deskline: no encoder reads the capturing GPU, encoding from memory: {e:#}");
                    }
                }
            }
            #[cfg(windows)]
            let made = hw::Encoder::new(w, h, bps, fps, None);
            #[cfg(target_os = "linux")]
            let made = hw::Encoder::new(w, h, bps, fps);
            match made {
                Ok(enc) => return hard(enc, 0),
                Err(e) => {
                    self.no_hw = true;
                    eprintln!("deskline: no hardware encoder, using OpenH264: {e:#}");
                }
            }
        }
        let _ = gpu;
        let config = EncoderConfig::new()
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(level_fps(w, h, fps) as f32))
            .complexity(Complexity::Low)
            .max_slice_len(16_000)
            // OpenH264's encoder threads share one pool per process, and a pool thread can still
            // be unlocking an encoder's task manager after that encoder is freed: a crash when
            // encoders restart (new size, new session). One thread costs ~1-3 ms a 1080p frame.
            .num_threads(1)
            .skip_frames(false);
        let enc = H264Encoder::with_api_config(OpenH264API::from_source(), config)?;
        Ok(Stream { enc: Backend::Soft(enc), w, h, bps, fps, gpu: 0 })
    }

    /// Step the size down when this computer can't keep the frame rate at it, and back up after
    /// a stretch with clear room.
    fn pace(&mut self, f: &Frame, took: Duration) {
        let p = &mut self.pace;
        p.frames += 1;
        p.busy += took;
        if p.frames < 10 || p.since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let ms = p.busy.as_secs_f64() * 1000.0 / p.frames as f64;
        (p.since, p.frames, p.busy) = (Instant::now(), 0, Duration::ZERO);
        let budget = 1000.0 / self.quality.fps() as f64;
        let steps = self.cpu_steps + self.net_step as u32;
        let smaller = fit(f.w, f.h, self.quality, steps + 1) != fit(f.w, f.h, self.quality, steps);
        if ms > budget * SLOW && smaller {
            self.cpu_steps += 1;
            p.calm = 0;
            p.up_after = (p.up_after * 2).min(64);
        } else if self.cpu_steps > 0 && ms * STEP_PIXELS < budget * ROOM {
            p.calm += 1;
            if p.calm >= p.up_after {
                self.cpu_steps -= 1;
                p.calm = 0;
            }
        } else {
            p.calm = 0;
        }
    }
}

impl Backend {
    fn encode(&mut self, p: Picture, key: bool) -> anyhow::Result<Vec<u8>> {
        match self {
            Backend::Soft(enc) => {
                let yuv = match p {
                    Picture::Yuv(yuv) => yuv,
                    #[cfg(any(windows, target_os = "linux"))]
                    _ => anyhow::bail!("OpenH264 takes I420 pictures"),
                };
                if key {
                    enc.force_intra_frame();
                }
                Ok(enc.encode(yuv)?.to_vec())
            }
            #[cfg(any(windows, target_os = "linux"))]
            Backend::Hard(enc, check) => {
                let out = match p {
                    #[cfg(windows)]
                    Picture::Yuv(yuv) => enc.encode(yuv, key)?,
                    #[cfg(windows)]
                    Picture::Gpu(tex) => enc.encode_gpu(tex, key)?,
                    #[cfg(target_os = "linux")]
                    Picture::Frame(f) => enc.encode(f, key)?,
                    #[cfg(target_os = "linux")]
                    Picture::Yuv(_) => anyhow::bail!("the GPU takes the frame as captured"),
                };
                // Trust it once OpenH264 has played its first picture at the right size.
                if let Some(dec) = check {
                    let pic = dec.decode(&out)?;
                    let size = pic.map(|(w, h, _)| (w, h));
                    anyhow::ensure!(size == Some(enc.size()), "OpenH264 got {size:?} from its first frame");
                    eprintln!("deskline: encoding on {}", enc.name);
                    *check = None;
                }
                Ok(out)
            }
        }
    }

    /// Change the bitrate mid-stream. False when the encoder won't.
    fn set_bitrate(&mut self, bps: u32, old: u32) -> bool {
        match self {
            Backend::Soft(enc) => {
                let mut set = |option, layer| {
                    let mut info = SBitrateInfo { iLayer: layer, iBitrate: bps as i32 };
                    // SAFETY: the bitrate options take an SBitrateInfo; the encoder has run.
                    unsafe { enc.raw_api().set_option(option, (&raw mut info).cast()) == 0 }
                };
                // OpenH264 refuses a target above the cap: raise the cap first, lower it last.
                if bps > old {
                    set(ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0) && set(ENCODER_OPTION_BITRATE, SPATIAL_LAYER_ALL)
                } else {
                    set(ENCODER_OPTION_BITRATE, SPATIAL_LAYER_ALL) && set(ENCODER_OPTION_MAX_BITRATE, SPATIAL_LAYER_0)
                }
            }
            #[cfg(any(windows, target_os = "linux"))]
            Backend::Hard(enc, _) => enc.set_bitrate(bps),
        }
    }
}

/// The encoded size of a `w`x`h` screen: the preset's long side, then `steps` of 2/3 (none
/// going below the quality's smallest), within OpenH264's limits, with even sides.
fn fit(w: usize, h: usize, q: Quality, steps: u32) -> (usize, usize) {
    let (long, short) = (w.max(h) as u64, w.min(h) as u64);
    // Scale by num / den.
    let (mut num, mut den) = (1, 1);
    let cap = q.preset().0;
    if long > cap {
        (num, den) = (cap, long);
    }
    if short * num > MAX_SHORT * den {
        (num, den) = (MAX_SHORT, short);
    }
    for _ in 0..steps {
        if long * num * STEP.0 < q.min_long() * den * STEP.1 {
            break;
        }
        (num, den) = (num * STEP.0, den * STEP.1);
    }
    let side = |v: usize| (v as u64 * num / den) as usize & !1;
    (side(w), side(h))
}

/// `fps`, or less where H.264 level 5.2 runs out at this size.
fn level_fps(w: usize, h: usize, fps: u32) -> u32 {
    let mbs = (w.div_ceil(16) * h.div_ceil(16)) as u64;
    (MAX_MBPS / mbs.max(1)).clamp(1, fps.into()) as u32
}

/// `f` at `w`x`h` (cropped to even sides, or area-averaged down) as I420, in `yuv`'s planes
/// when they're the right size.
fn to_yuv<'a>(
    f: &Frame,
    px: &[u8],
    w: usize,
    h: usize,
    scaler: &mut Option<Scaler>,
    scratch: &mut Vec<u8>,
    yuv: &'a mut Option<YUVBuffer>,
) -> &'a YUVBuffer {
    let px: &[u8] = if (w, h) == (f.w, f.h) {
        px
    } else if (w, h) == (f.w & !1, f.h & !1) {
        crop(px, f.w, w, h, scratch);
        scratch.as_slice()
    } else {
        if !matches!(scaler, Some(s) if s.from == (f.w, f.h) && s.to == (w, h)) {
            *scaler = Some(Scaler::new((f.w, f.h), (w, h)));
        }
        scaler.as_mut().expect("just set").run(px, scratch);
        scratch.as_slice()
    };
    if yuv.as_ref().is_none_or(|y| y.dimensions() != (w, h)) {
        *yuv = Some(YUVBuffer::new(w, h));
    }
    let out = yuv.as_mut().expect("just set");
    if f.bgra {
        out.read_bgra8(BgraSliceU8::new(px, (w, h)));
    } else {
        out.read_rgba8(RgbaSliceU8::new(px, (w, h)));
    }
    out
}

/// The top left `w`x`h` of a picture `stride` pixels wide.
fn crop(px: &[u8], stride: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.clear();
    for y in 0..h {
        out.extend_from_slice(&px[y * stride * 4..][..w * 4]);
    }
}

/// `f`'s pixels in memory, read back from the GPU when they're only there.
fn pixels(f: &Frame) -> anyhow::Result<Cow<'_, [u8]>> {
    #[cfg(windows)]
    if f.px.is_empty()
        && let Some(g) = &f.gpu
    {
        return Ok(Cow::Owned(g.read()?));
    }
    Ok(Cow::Borrowed(&f.px))
}

/// Area averaging down by any ratio: each output pixel is the mean of the source pixels it
/// covers, the partly covered ones weighted by how much. Rows first (a straight multiply-add
/// over whole rows, which vectorizes), then columns.
struct Scaler {
    from: (usize, usize),
    to: (usize, usize),
    rows: Vec<(usize, Vec<u16>)>,
    cols: Vec<(usize, Vec<u16>)>,
}

impl Scaler {
    fn new(from: (usize, usize), to: (usize, usize)) -> Self {
        Self { from, to, rows: spans(from.1, to.1), cols: spans(from.0, to.0) }
    }

    /// Bands of rows on four threads: 1440p to 1080p takes 12 ms on one core of a 2.4 GHz Xeon.
    fn run(&self, px: &[u8], out: &mut Vec<u8>) {
        let (dw, dh) = self.to;
        out.resize(dw * dh * 4, 0);
        let band = dh.div_ceil(4);
        std::thread::scope(|s| {
            for (rows, out) in self.rows.chunks(band).zip(out.chunks_mut(band * dw * 4)) {
                s.spawn(move || self.band(px, rows, out));
            }
        });
    }

    fn band(&self, px: &[u8], rows: &[(usize, Vec<u16>)], out: &mut [u8]) {
        let row_len = self.from.0 * 4;
        // One row scaled vertically, still at the source width, times 256.
        let mut acc = vec![0u16; row_len];
        for ((first, weights), line) in rows.iter().zip(out.chunks_exact_mut(self.to.0 * 4)) {
            acc.fill(0);
            for (k, &w) in weights.iter().enumerate() {
                // Weights sum to 256, so 255 * 256 at most: fits.
                for (a, &p) in acc.iter_mut().zip(&px[(first + k) * row_len..][..row_len]) {
                    *a += p as u16 * w;
                }
            }
            for ((first, weights), o) in self.cols.iter().zip(line.chunks_exact_mut(4)) {
                let mut sum = [1u32 << 15; 4];
                for (p, &w) in acc[first * 4..].chunks_exact(4).zip(weights) {
                    for c in 0..4 {
                        sum[c] += p[c] as u32 * w as u32;
                    }
                }
                for c in 0..4 {
                    o[c] = (sum[c] >> 16) as u8;
                }
            }
        }
    }
}

/// For each of `to` outputs over `from` inputs: the first input it covers and the weights of
/// the inputs it covers, which sum to 256.
fn spans(from: usize, to: usize) -> Vec<(usize, Vec<u16>)> {
    (0..to)
        .map(|o| {
            // In 1/to of an input pixel, output `o` covers [a, b) and input `i` covers
            // [i * to, (i + 1) * to). Rounding the running coverage keeps the sum exact.
            let (a, b) = (o * from, (o + 1) * from);
            let covered = |x: usize| ((x.clamp(a, b) - a) * 256 + from / 2) / from;
            let (first, last) = (a / to, (b - 1) / to);
            (first, (first..=last).map(|i| (covered((i + 1) * to) - covered(i * to)) as u16).collect())
        })
        .collect()
}

/// A hardware encoder's SPS and PPS: the profile checked on the first, both put back in front of
/// any keyframe that comes without them, so a viewer can always start from a keyframe.
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Headers(Vec<u8>);

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Headers {
    fn fix(&mut self, out: &mut Vec<u8>) -> anyhow::Result<()> {
        let (mut sps, mut idr) = (false, false);
        let mut found = Vec::new();
        for n in openh264::nal_units(out) {
            match nal_kind(n) {
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
            self.0 = found;
        } else if idr {
            anyhow::ensure!(!self.0.is_empty(), "keyframe without SPS");
            out.splice(0..0, self.0.iter().copied());
        }
        Ok(())
    }
}

/// NAL unit type of one Annex B unit (start code included).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn nal_kind(nal: &[u8]) -> Option<u8> {
    nal.iter().skip_while(|&&b| b != 1).nth(1).map(|b| b & 0x1f)
}

pub struct Decoder {
    h264: H264Decoder,
}

impl Decoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { h264: H264Decoder::new()? })
    }

    /// Returns `(width, height, rgba)` when the packet completes a picture. For checking the
    /// Windows hardware encoder's output, and tests; the window uses `decode_into`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn decode(&mut self, packet: &[u8]) -> anyhow::Result<Option<(usize, usize, Vec<u8>)>> {
        self.decode_into(packet, |w, h| vec![0; w * h * 4])
    }

    /// `decode`, writing RGBA straight into the buffer `make(w, h)` gives, which must be `w` *
    /// `h` * 4 bytes: the window's own image, with no copy in between.
    pub fn decode_into<B: AsMut<[u8]>>(
        &mut self,
        packet: &[u8],
        make: impl FnOnce(usize, usize) -> B,
    ) -> anyhow::Result<Option<(usize, usize, B)>> {
        let Some(yuv) = self.h264.decode(packet)? else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        let mut out = make(w, h);
        anyhow::ensure!(out.as_mut().len() == w * h * 4, "picture buffer is the wrong size");
        yuv.write_rgba8(out.as_mut());
        Ok(Some((w, h, out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, t: usize) -> Arc<Frame> {
        Arc::new(plain(w, h, t))
    }

    fn plain(w: usize, h: usize, t: usize) -> Frame {
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                // blue left half, red right half, a moving white bar
                let white = (x + t * 8) % w < 16;
                let (r, b) = if white { (255, 255) } else if x < w / 2 { (0, 255) } else { (255, 0) };
                px.extend_from_slice(&[b, if white { 255 } else { 0 }, r, 255]);
                let _ = y;
            }
        }
        Frame { w, h, bgra: true, px, gpu: None }
    }

    /// `frame` with its top quarter redrawn every frame with sharp edges and blocks that don't
    /// just move: what a rate change has to squeeze.
    fn busy(w: usize, h: usize, t: usize) -> Arc<Frame> {
        let mut f = plain(w, h, t);
        for (i, p) in f.px[..w * (h / 16) * 4].chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % w, i / w);
            let v = ((x * (t % 7 + 3) + y * (t % 5 + 2)) / 4) as u8;
            p[..3].copy_from_slice(&[v, v.wrapping_mul(3), v ^ (x >> 3) as u8]);
        }
        Arc::new(f)
    }

    /// OpenH264 only, whatever this computer has, at a size that stays put: a debug build
    /// sharing the CPU with other tests is slow enough to step down (pacing has its own test).
    fn soft(q: Quality) -> Encoder {
        let mut enc = Encoder { no_hw: true, ..Encoder::new(q) };
        enc.pace.since += Duration::from_secs(3600);
        enc
    }

    #[test]
    fn round_trip_keeps_size_and_colors() {
        let (mut enc, mut dec) = (soft(Quality::Balanced), Decoder::new().unwrap());
        let mut last = None;
        for t in 0..5 {
            let packet = enc.encode(&frame(320, 240, t), false).unwrap();
            assert!(!packet.is_empty(), "frame {t} encoded to nothing");
            last = dec.decode(&packet).unwrap().or(last);
            assert!(last.is_some(), "frame {t} did not decode right away");
        }
        let (w, h, rgba) = last.unwrap();
        assert_eq!((w, h), (320, 240));
        // pixel well inside the left half is blue, right half red
        let at = |x: usize, y: usize| &rgba[(y * w + x) * 4..][..3];
        let (l, r) = (at(100, 120), at(250, 120));
        assert!(l[2] > 200 && l[0] < 60, "left should be blue, got {l:?}");
        assert!(r[0] > 200 && r[2] < 60, "right should be red, got {r:?}");
        assert!(!enc.hardware());
    }

    /// `cargo test --release encode_speed -- --ignored --nocapture`. Must stay well under 33 ms.
    #[test]
    #[ignore]
    fn encode_speed_1080p() {
        let frames: Vec<_> = (0..60).map(|t| frame(1920, 1080, t)).collect();
        let t = std::time::Instant::now();
        for f in &frames {
            let _ = std::hint::black_box(YUVBuffer::from_bgra8_source(BgraSliceU8::new(&f.px, (f.w, f.h))));
        }
        println!("1080p bgra->yuv: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / 60.0);
        let scaler = Scaler::new((2560, 1440), (1920, 1080));
        let (big, mut out) = (frame(2560, 1440, 0), Vec::new());
        let t = std::time::Instant::now();
        for _ in 0..30 {
            scaler.run(&big.px, &mut out);
        }
        println!("1440p->1080p scale: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / 30.0);

        for q in [Quality::Smooth, Quality::Balanced, Quality::Sharp] {
            let mut enc = soft(q);
            let t = std::time::Instant::now();
            let bytes: usize = frames.iter().map(|f| enc.encode(f, false).unwrap().len()).sum();
            let ms = t.elapsed().as_secs_f64() * 1000.0 / 60.0;
            println!("1080p {q:?}: {ms:.1} ms/frame, {} kbit/s at 30 fps", bytes * 8 * 30 / 60 / 1000);
            assert!(ms < 33.0, "too slow for 30 fps: {ms:.1} ms");
        }
        let noisy: Vec<_> = (0..30).map(|t| busy(1920, 1080, t)).collect();
        for q in [Quality::Smooth, Quality::Balanced, Quality::Sharp] {
            let mut enc = soft(q);
            for rate in [1.0, 0.5] {
                enc.set_rate(rate);
                let bytes: usize = noisy.iter().map(|f| enc.encode(f, false).unwrap().len()).sum();
                println!("1080p busy {q:?} at rate {rate}: {} kbit/s", bytes * 8 / 1000);
            }
        }

        // 4K Sharp: full size if this CPU keeps up, a step down if not.
        let big: Vec<_> = (0..4).map(|t| frame(3840, 2160, t)).collect();
        let mut enc = Encoder { no_hw: true, ..Encoder::new(Quality::Sharp) };
        let t = std::time::Instant::now();
        for i in 0..90 {
            enc.encode(&big[i % 4], false).unwrap();
        }
        let s = enc.stream.as_ref().unwrap();
        println!("4K Sharp: {:.1} ms/frame over 90 frames, settled at {}x{}", t.elapsed().as_secs_f64() * 1000.0 / 90.0, s.w, s.h);
    }

    #[test]
    fn sizes_per_quality() {
        use Quality::*;
        let want = [
            ((1280, 720), [(1280, 720), (1280, 720), (1280, 720)]),
            ((1920, 1080), [(1920, 1080), (1920, 1080), (1280, 720)]),
            ((2560, 1440), [(1920, 1080), (2560, 1440), (1280, 720)]),
            ((3840, 2160), [(1920, 1080), (3840, 2160), (1280, 720)]),
            ((3440, 1440), [(1920, 802), (3440, 1440), (1280, 534)]),
            ((5120, 2880), [(1920, 1080), (3840, 2160), (1280, 720)]),
            ((1080, 1920), [(1080, 1920), (1080, 1920), (720, 1280)]),
            ((641, 481), [(640, 480), (640, 480), (640, 480)]),
            ((2561, 1441), [(1920, 1080), (2560, 1440), (1280, 720)]),
        ];
        for ((w, h), sizes) in want {
            for (q, size) in [Balanced, Sharp, Smooth].into_iter().zip(sizes) {
                assert_eq!(fit(w, h, q, 0), size, "{w}x{h} {q:?}");
            }
        }
        // Steps are 2/3 each and stop above 640.
        assert_eq!(fit(1920, 1080, Balanced, 1), (1280, 720));
        assert_eq!(fit(1920, 1080, Balanced, 2), (852, 480));
        assert_eq!(fit(1920, 1080, Balanced, 9), (852, 480));
        assert_eq!(fit(3840, 2160, Sharp, 1), (2560, 1440));
        assert_eq!(fit(640, 360, Balanced, 3), (640, 360));
    }

    #[test]
    fn each_quality_decodes_at_its_size() {
        for (q, size) in [(Quality::Balanced, (1920, 1080)), (Quality::Sharp, (2560, 1440)), (Quality::Smooth, (1280, 720))] {
            let (mut enc, mut dec) = (soft(q), Decoder::new().unwrap());
            let (w, h, _) = dec.decode(&enc.encode(&frame(2560, 1440, 0), true).unwrap()).unwrap().unwrap();
            assert_eq!((w, h), size, "{q:?}");
        }
    }

    #[test]
    fn wide_screens_are_scaled_and_odd_sizes_cropped() {
        let (mut enc, mut dec) = (soft(Quality::Balanced), Decoder::new().unwrap());
        let (w, h, rgba) = dec.decode(&enc.encode(&frame(2560, 1440, 0), true).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (1920, 1080));
        let (l, r) = (&rgba[(540 * w + 400) * 4..][..3], &rgba[(540 * w + 1500) * 4..][..3]);
        assert!(l[2] > 200 && l[0] < 60 && r[0] > 200 && r[2] < 60, "colors lost in scaling: {l:?} {r:?}");
        let (w, h, _) = dec.decode(&enc.encode(&frame(641, 481, 0), false).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (640, 480));
    }

    #[test]
    fn area_average_keeps_flat_colors_and_means() {
        // 3 -> 2: the middle pixel is split half and half.
        assert_eq!(spans(3, 2), vec![(0, vec![171, 85]), (1, vec![85, 171])]);
        assert_eq!(spans(4, 2), vec![(0, vec![128, 128]), (2, vec![128, 128])]);
        let f = Frame { w: 5, h: 3, bgra: true, px: [10, 20, 30, 255].repeat(15), gpu: None };
        let mut out = Vec::new();
        Scaler::new((5, 3), (2, 2)).run(&f.px, &mut out);
        assert_eq!(out, [10, 20, 30, 255].repeat(4));
        // Black and white columns average to grey.
        let px = (0..4 * 2).flat_map(|i| if i % 2 == 0 { [0; 4] } else { [255; 4] }).collect::<Vec<u8>>();
        Scaler::new((4, 2), (2, 1)).run(&px, &mut out);
        assert_eq!(out, [128, 128, 128, 128].repeat(2));
    }

    #[test]
    fn set_rate_lowers_bitrate_without_breaking_the_stream() {
        let (mut enc, mut dec) = (soft(Quality::Balanced), Decoder::new().unwrap());
        let frames: Vec<_> = (0..45).map(|t| busy(1280, 720, t)).collect();
        let mut sizes = Vec::new();
        for rate in [1.0, 0.4] {
            enc.set_rate(rate);
            let mut bytes = 0;
            for (t, f) in frames.iter().enumerate() {
                let packet = enc.encode(f, false).unwrap();
                let sps = openh264::nal_units(&packet).any(|n| n.iter().skip_while(|&&b| b != 1).nth(1).map(|b| b & 0x1f) == Some(7));
                assert_eq!(sps, rate == 1.0 && t == 0, "a new stream started at frame {t}: the rate didn't change in place");
                // The rate control takes a few frames to settle: count the last second.
                if t >= 15 {
                    bytes += packet.len();
                }
                let (w, h, _) = dec.decode(&packet).unwrap().expect("every frame decodes");
                assert_eq!((w, h), (1280, 720));
            }
            sizes.push(bytes);
        }
        assert!(sizes[1] * 10 < sizes[0] * 7, "bytes didn't drop: {sizes:?}");

        // At 0.35 and below the picture steps down a size, and comes back at 0.5.
        enc.set_rate(0.3);
        let (w, _, _) = dec.decode(&enc.encode(&busy(1920, 1080, 0), false).unwrap()).unwrap().unwrap();
        assert_eq!(w, 1280);
        enc.set_rate(0.45);
        let (w, _, _) = dec.decode(&enc.encode(&busy(1920, 1080, 1), false).unwrap()).unwrap().unwrap();
        assert_eq!(w, 1280, "no flip back before 0.5");
        enc.set_rate(0.6);
        let (w, _, _) = dec.decode(&enc.encode(&busy(1920, 1080, 2), false).unwrap()).unwrap().unwrap();
        assert_eq!(w, 1920);
    }

    #[test]
    fn gaming_stays_within_the_level_and_above_720p() {
        // Level 5.2 holds 4K at 64 fps, 1080p at 120.
        assert_eq!(level_fps(3840, 2160, 120), 64);
        assert_eq!(level_fps(1920, 1080, 120), 120);
        assert_eq!(level_fps(1920, 1080, 30), 30);
        assert_eq!(fit(3840, 2160, Quality::Gaming, 0), (3840, 2160));
        assert_eq!(fit(3840, 2160, Quality::Gaming, 9), (1706, 960));
        // 8.3 ms a frame at 120 fps: 9 ms is too slow.
        let mut enc = soft(Quality::Gaming);
        let f = frame(3840, 2160, 0);
        enc.pace.since = Instant::now() - Duration::from_secs(2);
        for _ in 0..10 {
            enc.pace(&f, Duration::from_millis(9));
        }
        assert_eq!(enc.cpu_steps, 1);
    }

    #[test]
    fn slow_encodes_step_down_and_room_steps_back_up() {
        let mut enc = soft(Quality::Balanced);
        let f = frame(1920, 1080, 0);
        let window = |enc: &mut Encoder, ms| {
            enc.pace.since = Instant::now() - Duration::from_secs(2);
            for _ in 0..10 {
                enc.pace(&f, Duration::from_millis(ms));
            }
        };
        window(&mut enc, 40);
        assert_eq!(enc.cpu_steps, 1);
        window(&mut enc, 12);
        assert_eq!(enc.cpu_steps, 1, "12 ms here would be 27 ms a size up: no room");
        for _ in 0..5 {
            window(&mut enc, 4);
        }
        assert_eq!(enc.cpu_steps, 1, "waits longer after each step down");
        window(&mut enc, 4);
        assert_eq!(enc.cpu_steps, 0);
    }

    /// The GPU's encoder, where there is one: `cargo test hardware -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn hardware_output_plays_in_openh264() {
        let mut enc = Encoder::default();
        let mut dec = Decoder::new().unwrap();
        let (mut bytes, mut took) = (0, Duration::ZERO);
        for i in 0..60 {
            let f = frame(1920, 1080, i);
            let t = Instant::now();
            let packet = enc.encode(&f, i == 30).unwrap();
            if i > 0 {
                took += t.elapsed();
            }
            bytes += packet.len();
            let (w, h, _) = dec.decode(&packet).unwrap().expect("every frame decodes");
            assert_eq!((w, h), (1920, 1080));
        }
        println!("hardware: {}, {:.1} ms/frame, {} kbit/s", enc.hardware(), took.as_secs_f64() * 1000.0 / 59.0, bytes * 8 / 2 / 1000);
        assert!(enc.hardware(), "no hardware encoder here, or it failed (see stderr)");
        enc.set_rate(0.5);
        enc.encode(&frame(1920, 1080, 61), false).unwrap();
        // An odd size after a step down still works, and a fresh decoder can start on a keyframe.
        enc.set_rate(0.2);
        let mut fresh = Decoder::new().unwrap();
        let (w, h, _) = fresh.decode(&enc.encode(&frame(1920, 1080, 62), true).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (1280, 720));
        let mut fresh = Decoder::new().unwrap();
        let (w, h, _) = fresh.decode(&enc.encode(&frame(1700, 956, 63), true).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (1132, 636));
        assert!(enc.hardware());
    }
}
