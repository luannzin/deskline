//! H.264 both ways. Screens wider than 1920 are halved first so a software encoder keeps 30 fps.
use openh264::OpenH264API;
use openh264::decoder::Decoder as H264Decoder;
use openh264::encoder::{BitRate, Complexity, Encoder as H264Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, RgbaSliceU8, YUVBuffer, YUVSource};

use crate::desktop::Frame;

#[derive(Default)]
pub struct Encoder {
    h264: Option<(H264Encoder, usize, usize)>,
    scratch: Vec<u8>,
}

impl Encoder {
    /// Encode one frame; `key` forces a keyframe. A new size starts a new stream (keyframe
    /// with fresh SPS), which the decoder follows on its own.
    pub fn encode(&mut self, f: &Frame, key: bool) -> anyhow::Result<Vec<u8>> {
        let half = f.w > 1920;
        let (w, h) = if half { (f.w / 2, f.h / 2) } else { (f.w, f.h) };
        let (w, h) = (w & !1, h & !1);
        anyhow::ensure!(w >= 16 && h >= 16, "frame too small: {}x{}", f.w, f.h);

        let px: &[u8] = if half {
            halve(f, w, h, &mut self.scratch);
            &self.scratch
        } else if w == f.w && h == f.h {
            &f.px
        } else {
            crop(f, w, h, &mut self.scratch);
            &self.scratch
        };
        let yuv = if f.bgra {
            YUVBuffer::from_bgra8_source(BgraSliceU8::new(px, (w, h)))
        } else {
            YUVBuffer::from_rgba8_source(RgbaSliceU8::new(px, (w, h)))
        };

        if !matches!(self.h264, Some((_, ew, eh)) if (ew, eh) == (w, h)) {
            let config = EncoderConfig::new()
                .usage_type(UsageType::CameraVideoRealTime)
                .rate_control_mode(RateControlMode::Bitrate)
                .bitrate(BitRate::from_bps((w * h * 2) as u32))
                .max_frame_rate(FrameRate::from_hz(30.0))
                .complexity(Complexity::Low)
                .max_slice_len(16_000)
                .num_threads(4)
                .skip_frames(false);
            self.h264 = Some((H264Encoder::with_api_config(OpenH264API::from_source(), config)?, w, h));
        }
        let (enc, _, _) = self.h264.as_mut().expect("just set");
        if key {
            enc.force_intra_frame();
        }
        Ok(enc.encode(&yuv)?.to_vec())
    }
}

/// 2x2 box filter.
fn halve(f: &Frame, w: usize, h: usize, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(w * h * 4);
    let row = f.w * 4;
    for y in 0..h {
        let (a, b) = (&f.px[2 * y * row..], &f.px[(2 * y + 1) * row..]);
        for x in 0..w {
            let i = 8 * x;
            for c in 0..4 {
                let s = a[i + c] as u16 + a[i + 4 + c] as u16 + b[i + c] as u16 + b[i + 4 + c] as u16;
                out.push((s / 4) as u8);
            }
        }
    }
}

fn crop(f: &Frame, w: usize, h: usize, out: &mut Vec<u8>) {
    out.clear();
    for y in 0..h {
        out.extend_from_slice(&f.px[y * f.w * 4..][..w * 4]);
    }
}

pub struct Decoder {
    h264: H264Decoder,
}

impl Decoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { h264: H264Decoder::new()? })
    }

    /// Returns `(width, height, rgba)` when the packet completes a picture.
    pub fn decode(&mut self, packet: &[u8]) -> anyhow::Result<Option<(usize, usize, Vec<u8>)>> {
        let Some(yuv) = self.h264.decode(packet)? else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        let mut rgba = vec![0; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        Ok(Some((w, h, rgba)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, t: usize) -> Frame {
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
        Frame { w, h, bgra: true, px }
    }

    #[test]
    fn round_trip_keeps_size_and_colors() {
        let (mut enc, mut dec) = (Encoder::default(), Decoder::new().unwrap());
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
    }

    /// `cargo test --release encode_speed -- --ignored --nocapture`. Must stay well under 33 ms.
    #[test]
    #[ignore]
    fn encode_speed_1080p() {
        let mut enc = Encoder::default();
        let frames: Vec<_> = (0..60).map(|t| frame(1920, 1080, t)).collect();
        let t = std::time::Instant::now();
        for f in &frames {
            std::hint::black_box(YUVBuffer::from_bgra8_source(BgraSliceU8::new(&f.px, (f.w, f.h))));
        }
        println!("1080p bgra->yuv: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1000.0 / 60.0);
        let t = std::time::Instant::now();
        let bytes: usize = frames.iter().map(|f| enc.encode(f, false).unwrap().len()).sum();
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 60.0;
        println!("1080p: {ms:.1} ms/frame, {} kbit/s at 30 fps", bytes * 8 * 30 / 60 / 1000);
        assert!(ms < 33.0, "too slow for 30 fps: {ms:.1} ms");
    }

    #[test]
    fn wide_screens_are_halved_and_odd_sizes_cropped() {
        let (mut enc, mut dec) = (Encoder::default(), Decoder::new().unwrap());
        let (w, h, _) = dec.decode(&enc.encode(&frame(2560, 1440, 0), true).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (1280, 720));
        let (w, h, _) = dec.decode(&enc.encode(&frame(641, 481, 0), false).unwrap()).unwrap().unwrap();
        assert_eq!((w, h), (640, 480));
    }
}
