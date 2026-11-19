//! Linux: the GPU's H.264 encoder through GStreamer. NVENC (nvh264enc) on NVIDIA, VA-API on
//! Intel and AMD (vah264enc, or the older vaapih264enc). The GPU also scales the captured picture
//! and converts it to NV12; the frame's pixels go to it as they are, without a copy. Without
//! those elements (gstreamer1.0-plugins-bad, gstreamer1.0-vaapi) OpenH264 encodes instead.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

use super::Headers;
use crate::desktop::Frame;

/// A hardware encoder answers in a few ms; this long means it's stuck or holding frames back.
const PATIENCE: Duration = Duration::from_millis(500);
/// The first picture also starts the GPU's pipeline.
const STARTUP: Duration = Duration::from_secs(3);

/// One way to encode on a GPU: the elements that upload, scale and convert the picture to NV12,
/// the memory they leave it in, and the encoder with its low-latency settings.
struct Kind {
    encoder: &'static str,
    convert: &'static [&'static str],
    memory: &'static str,
    settings: &'static [(&'static str, &'static str)],
}

/// Best first. Settings an element doesn't have (older versions) are skipped.
const KINDS: [Kind; 3] = [
    Kind {
        encoder: "nvh264enc",
        convert: &["cudaupload", "cudaconvertscale"],
        memory: "memory:CUDAMemory",
        settings: &[("preset", "low-latency-hq"), ("rc-mode", "cbr"), ("gop-size", "-1"), ("bframes", "0"), ("zerolatency", "true")],
    },
    Kind {
        encoder: "vah264enc",
        convert: &["vapostproc"],
        memory: "memory:VAMemory",
        settings: &[("rate-control", "cbr"), ("key-int-max", "1024"), ("b-frames", "0"), ("ref-frames", "1"), ("target-usage", "7")],
    },
    Kind {
        encoder: "vaapih264enc",
        convert: &["vaapipostproc"],
        memory: "memory:VASurface",
        settings: &[("rate-control", "cbr"), ("keyframe-period", "1024"), ("max-bframes", "0")],
    },
];

pub struct Encoder {
    pipeline: gst::Pipeline,
    src: AppSrc,
    sink: AppSink,
    enc: gst::Element,
    w: usize,
    h: usize,
    fps: u32,
    /// The capture the source's caps describe: width, height, BGRx (or RGBx).
    from: Option<(usize, usize, bool)>,
    frames: u64,
    headers: Headers,
    pub name: String,
}

impl Encoder {
    /// The first GPU encoder that gives Constrained Baseline H.264 at `w`x`h`, `bps` and `fps`.
    pub fn new(w: usize, h: usize, bps: u32, fps: u32) -> anyhow::Result<Self> {
        // Returns at once after the first time.
        gst::init()?;
        let mut why = Vec::new();
        for k in KINDS.iter().filter(|k| gst::ElementFactory::find(k.encoder).is_some()) {
            match Self::open(k, w, h, bps, fps) {
                Ok(e) => return Ok(e),
                Err(e) => why.push(format!("{}: {e:#}", k.encoder)),
            }
        }
        anyhow::ensure!(!why.is_empty(), "no GStreamer GPU encoder (gstreamer1.0-plugins-bad or gstreamer1.0-vaapi)");
        anyhow::bail!("{}", why.join("; "))
    }

    fn open(k: &Kind, w: usize, h: usize, bps: u32, fps: u32) -> anyhow::Result<Self> {
        let src = AppSrc::builder().format(gst::Format::Time).is_live(true).build();
        let enc = gst::ElementFactory::make(k.encoder).build()?;
        for (name, value) in k.settings {
            set(&enc, name, value);
        }
        set(&enc, "bitrate", &(bps / 1000).to_string());
        let sink = AppSink::builder().sync(false).max_buffers(4).build();
        let mut chain: Vec<gst::Element> = vec![src.clone().upcast()];
        for c in k.convert {
            chain.push(gst::ElementFactory::make(c).build()?);
        }
        chain.push(capsfilter(&format!("video/x-raw({}),format=NV12,width={w},height={h}", k.memory))?);
        chain.push(enc.clone());
        chain.push(capsfilter("video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline")?);
        chain.push(sink.clone().upcast());
        let pipeline = gst::Pipeline::new();
        pipeline.add_many(&chain)?;
        gst::Element::link_many(&chain)?;
        pipeline.set_state(gst::State::Playing)?;
        let name = format!("{} (GStreamer)", k.encoder);
        Ok(Self { pipeline, src, sink, enc, w, h, fps, from: None, frames: 0, headers: Headers::default(), name })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Change the bitrate without restarting. False when the encoder won't while running.
    pub fn set_bitrate(&mut self, bps: u32) -> bool {
        let live = self.enc.find_property("bitrate").is_some_and(|p| p.flags().contains(gst::PARAM_FLAG_MUTABLE_PLAYING));
        live && set(&self.enc, "bitrate", &(bps / 1000).to_string())
    }

    /// One captured frame in (any size), its H.264 out. Errors if the encoder holds it back.
    pub fn encode(&mut self, f: &Arc<Frame>, key: bool) -> anyhow::Result<Vec<u8>> {
        if self.from != Some((f.w, f.h, f.bgra)) {
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", if f.bgra { "BGRx" } else { "RGBx" })
                .field("width", f.w as i32)
                .field("height", f.h as i32)
                .field("framerate", gst::Fraction::new(self.fps as i32, 1))
                .build();
            self.src.set_caps(Some(&caps));
            self.from = Some((f.w, f.h, f.bgra));
        }
        if key {
            self.src.send_event(gstreamer_video::DownstreamForceKeyUnitEvent::builder().all_headers(true).build());
        }
        let frame = gst::ClockTime::SECOND / u64::from(self.fps);
        let mut buffer = gst::Buffer::from_slice(Pixels(f.clone()));
        let b = buffer.get_mut().expect("a new buffer is writable");
        b.set_pts(frame * self.frames);
        b.set_duration(frame);
        self.src.push_buffer(buffer).map_err(|e| anyhow::anyhow!("the encoder refused the picture: {e:?}"))?;
        let patience = if self.frames == 0 { STARTUP } else { PATIENCE };
        self.frames += 1;
        let sample = self.next(patience)?;
        let mut out = sample.buffer().context("an empty sample")?.map_readable()?.to_vec();
        self.headers.fix(&mut out)?;
        Ok(out)
    }

    /// The next encoded picture, or the pipeline's error if it has one.
    fn next(&self, patience: Duration) -> anyhow::Result<gst::Sample> {
        let deadline = Instant::now() + patience;
        let bus = self.pipeline.bus().context("no bus")?;
        loop {
            if let Some(s) = self.sink.try_pull_sample(gst::ClockTime::from_mseconds(20)) {
                return Ok(s);
            }
            // Read everything on the bus, so it doesn't pile up; an error ends the stream.
            while let Some(m) = bus.pop() {
                if let gst::MessageView::Error(e) = m.view() {
                    anyhow::bail!("{}", e.error());
                }
            }
            anyhow::ensure!(Instant::now() < deadline, "no answer in {} ms", patience.as_millis());
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// A frame's pixels as a GStreamer buffer: the frame stays alive while the GPU reads them.
struct Pixels(Arc<Frame>);

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0.px
    }
}

fn capsfilter(caps: &str) -> anyhow::Result<gst::Element> {
    Ok(gst::ElementFactory::make("capsfilter").property("caps", caps.parse::<gst::Caps>()?).build()?)
}

/// Set a property from its text form if the element has it, can change it and the value fits:
/// versions and vendors differ in what they offer (glib-rs panics on the rest).
fn set(e: &gst::Element, name: &str, value: &str) -> bool {
    use glib::translate::{ToGlibPtr, ToGlibPtrMut};
    let Some(p) = e.find_property(name) else { return false };
    let flags = p.flags();
    if !flags.contains(glib::ParamFlags::WRITABLE) || flags.contains(glib::ParamFlags::CONSTRUCT_ONLY) {
        return false;
    }
    let Ok(mut v) = glib::Value::deserialize(value, p.value_type()) else { return false };
    // SAFETY: a live pspec and a value of its type; validating may only clamp the value.
    let clamped = unsafe { glib::gobject_ffi::g_param_value_validate(p.to_glib_none().0, v.to_glib_none_mut().0) };
    if clamped != glib::ffi::GFALSE {
        return false;
    }
    e.set_property_from_value(name, &v);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pipeline end to end with x264 standing in for the GPU (none in CI):
    /// `cargo test gstreamer -- --ignored` with gstreamer1.0-plugins-ugly installed.
    #[test]
    #[ignore]
    fn gstreamer_pipeline_scales_encodes_and_plays() {
        let kind = Kind {
            encoder: "x264enc",
            convert: &["videoconvert", "videoscale"],
            memory: "memory:SystemMemory",
            settings: &[("tune", "zerolatency"), ("speed-preset", "ultrafast"), ("key-int-max", "1024"), ("bogus", "1"), ("bitrate", "-5")],
        };
        gst::init().unwrap();
        let mut enc = Encoder::open(&kind, 640, 360, 2_000_000, 30).unwrap();
        let mut dec = super::super::Decoder::new().unwrap();
        for t in 0..10 {
            let px = (0..1280 * 720).flat_map(|i| [(i % 1280 + t * 8) as u8, (i / 1280) as u8, 128, 255]).collect();
            let f = Arc::new(Frame { w: 1280, h: 720, bgra: true, px, gpu: None });
            let out = enc.encode(&f, t == 5).unwrap();
            let (w, h, _) = dec.decode(&out).unwrap().expect("every output plays");
            assert_eq!((w, h), (640, 360));
        }
        assert!(enc.set_bitrate(1_000_000), "x264enc changes its bitrate while running");
    }
}
