//! The host's sound to the viewer. The host captures what its speakers play (not the
//! microphone) and sends one packet per 20 ms: an Opus frame, 48 kHz stereo, ~64 kbit/s, small
//! enough for one QUIC datagram. The viewer decodes and plays them behind a small jitter buffer.
//!
//! - Windows: WASAPI loopback of the default output device.
//! - Linux: a PipeWire capture stream on the default sink's monitor. Playback goes through ALSA,
//!   which PipeWire and PulseAudio both serve.
//! - macOS: an installed loopback device (BlackHole, Loopback, Soundflower), else Core Audio's
//!   system audio tap (macOS 14.6 and later).
//!
//! Opus is `unsafe_libopus`: libopus 1.3.1 machine-translated to Rust and checked against the
//! reference test vectors. No C compiler or CMake, so MSVC, Linux, macOS and the mingw cross
//! build all just work. It's ~20% slower than C, which 64 kbit/s stereo never notices.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
#[cfg(target_os = "linux")]
use pipewire as pw;
use tokio::sync::mpsc;
use unsafe_libopus as opus;

const RATE: u32 = 48_000;
/// 20 ms at 48 kHz, per channel.
const FRAME: usize = 960;
/// The longest Opus packet (120 ms), per channel: anything valid decodes.
const MAX_FRAME: usize = 5760;
const BITRATE: i32 = 64_000;
/// Opus keeps every packet under this, so packet plus the caller's header fit one datagram.
const MAX_PACKET: usize = 1000;
/// After this many silent frames (0.5 s) nothing is sent until there's sound again.
const QUIET_FRAMES: usize = 25;
/// Quieter than one 16-bit step counts as digital silence.
const SILENCE: f32 = 1.0 / 32768.0;

/// Capturing what this computer plays. Dropping it stops.
pub struct Capture {
    #[cfg(target_os = "linux")]
    stop: pw::channel::Sender<()>,
    #[cfg(not(target_os = "linux"))]
    stop: std_mpsc::Sender<()>,
}

impl Capture {
    /// Packets: one Opus frame each (20 ms, 48 kHz stereo), small enough for one QUIC datagram.
    /// Nothing is sent while the computer is silent; the channel closes if the device goes away.
    /// Blocks until the device is open (normally well under a second).
    pub fn start() -> anyhow::Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        // 320 ms. When it's full packets are dropped: the sound thread must never wait.
        let (tx, rx) = mpsc::channel(16);
        let stop = Packer::new(tx).and_then(capture).map_err(|e| anyhow::anyhow!("Couldn't capture this computer's sound: {e}"))?;
        Ok((Self { stop }, rx))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

/// Plays packets from `Capture` on the default output device. Dropping it stops.
pub struct Player {
    jitter: Arc<Mutex<Jitter>>,
    stop: std_mpsc::Sender<()>,
}

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        let (stop, jitter) = on_thread(|_| {
            let device = cpal::default_host().default_output_device().context("this computer has no speakers or other sound output")?;
            let config = device.default_output_config()?;
            let jitter = Arc::new(Mutex::new(Jitter::new(config.sample_rate())?));
            let (j, cfg) = (jitter.clone(), config.config());
            let stream = match config.sample_format() {
                SampleFormat::F32 => output::<f32>(&device, cfg, j),
                SampleFormat::I16 => output::<i16>(&device, cfg, j),
                SampleFormat::I32 => output::<i32>(&device, cfg, j),
                SampleFormat::U16 => output::<u16>(&device, cfg, j),
                f => anyhow::bail!("the speakers use a sample format Deskline can't write ({f:?})"),
            }?;
            Ok((stream, jitter))
        })
        .map_err(|e| anyhow::anyhow!("Couldn't play the other computer's sound: {e}"))?;
        Ok(Self { jitter, stop })
    }

    /// Queue one packet. Late or lost packets are fine; the queue never grows past ~150 ms.
    pub fn push(&mut self, packet: &[u8]) {
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).packet(packet);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

/// Opens a cpal stream on a thread of its own and keeps it there until the returned sender
/// fires. Off tokio's threads on purpose: on Windows cpal makes the thread that opens a device
/// single-threaded COM, which would break any other COM user on that worker.
fn on_thread<R: Send + 'static>(
    open: impl FnOnce(std_mpsc::Sender<()>) -> anyhow::Result<(cpal::Stream, R)> + Send + 'static,
) -> anyhow::Result<(std_mpsc::Sender<()>, R)> {
    let (stop, stopped) = std_mpsc::channel();
    let (ready_tx, ready) = std_mpsc::channel();
    let s = stop.clone();
    std::thread::Builder::new().name("audio".into()).spawn(move || {
        let opened = open(s).and_then(|(stream, r)| {
            stream.play()?;
            Ok((stream, r))
        });
        match opened {
            Ok((stream, r)) => {
                let _ = ready_tx.send(Ok(r));
                let _ = stopped.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    let r = ready.recv().context("the sound thread stopped")??;
    Ok((stop, r))
}

// ---------------------------------------------------------------- capture

#[cfg(not(target_os = "linux"))]
fn capture(packer: Packer) -> anyhow::Result<std_mpsc::Sender<()>> {
    let opened = on_thread(move |stop| {
        let (device, config) = loopback()?;
        let cfg = config.config();
        let stream = match config.sample_format() {
            SampleFormat::F32 => input::<f32>(&device, cfg, packer, stop),
            SampleFormat::I16 => input::<i16>(&device, cfg, packer, stop),
            SampleFormat::I32 => input::<i32>(&device, cfg, packer, stop),
            SampleFormat::U16 => input::<u16>(&device, cfg, packer, stop),
            f => anyhow::bail!("the sound device uses a sample format Deskline can't read ({f:?})"),
        }?;
        Ok((stream, ()))
    });
    #[cfg(target_os = "macos")]
    let opened = opened.map_err(|_| anyhow::anyhow!("sharing sound on a Mac needs macOS 14.6 or later, or a loopback device such as BlackHole"));
    Ok(opened?.0)
}

/// What the speakers play: the default output device opened for input (WASAPI loopback, or
/// Core Audio's tap on a Mac). On a Mac an installed loopback device comes first: whoever
/// installed one routes their sound through it.
#[cfg(not(target_os = "linux"))]
fn loopback() -> anyhow::Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    let host = cpal::default_host();
    #[cfg(target_os = "macos")]
    for d in host.input_devices()? {
        let name = d.description().map(|d| d.name().to_owned()).unwrap_or_default();
        if ["BlackHole", "Loopback", "Soundflower"].iter().any(|k| name.contains(k)) {
            let config = d.default_input_config()?;
            return Ok((d, config));
        }
    }
    let d = host.default_output_device().context("this computer has no speakers or other sound output")?;
    let config = d.default_output_config()?;
    Ok((d, config))
}

#[cfg(not(target_os = "linux"))]
fn input<T>(device: &cpal::Device, config: cpal::StreamConfig, mut packer: Packer, stop: std_mpsc::Sender<()>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let (ch, rate) = (config.channels.max(1) as usize, config.sample_rate);
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            packer.push(rate, data.chunks_exact(ch).map(|f| stereo(ch, |i| f[i].to_sample())));
        },
        // Ending the stream drops the packer, which closes the packet channel. Only when the
        // device is gone for good, not for a glitch.
        move |e| {
            use cpal::ErrorKind::*;
            if matches!(e.kind(), DeviceNotAvailable | StreamInvalidated | HostUnavailable) {
                let _ = stop.send(());
            }
        },
        None,
    )
}

/// The default sink's monitor through PipeWire, as 48 kHz stereo floats (PipeWire converts).
#[cfg(target_os = "linux")]
fn capture(packer: Packer) -> anyhow::Result<pw::channel::Sender<()>> {
    let (stop, stopped) = pw::channel::channel();
    let (ready_tx, ready) = std_mpsc::channel();
    std::thread::Builder::new().name("pipewire-sound".into()).spawn(move || {
        if let Err(e) = pipewire_capture(packer, stopped, ready_tx.clone()) {
            let _ = ready_tx.send(Err(e));
        }
    })?;
    // Streaming means it's linked to the speakers. A sound server that isn't PipeWire (plain
    // PulseAudio) never links it.
    match ready.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(stop),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let _ = stop.send(());
            anyhow::bail!("PipeWire didn't connect it to any speakers")
        }
    }
}

/// Runs the PipeWire loop on this thread until `stop` fires or the stream dies.
#[cfg(target_os = "linux")]
fn pipewire_capture(packer: Packer, stop: pw::channel::Receiver<()>, ready: std_mpsc::Sender<anyhow::Result<()>>) -> anyhow::Result<()> {
    use pw::spa;
    use spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
    use spa::pod::{Object, Pod, Value, serialize::PodSerializer};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("PipeWire isn't running")?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "Deskline",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            // The default sink's monitor: what the speakers play, not the microphone.
            *pw::keys::STREAM_CAPTURE_SINK => "true",
        },
    )?;

    let quit = mainloop.clone();
    let _stop = stop.attach(mainloop.loop_(), move |_| quit.quit());

    let quit = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data((packer, AudioInfoRaw::new()))
        .state_changed(move |_, _, _, new| match new {
            pw::stream::StreamState::Streaming => {
                let _ = ready.send(Ok(()));
            }
            // The sound server went away: ending the loop drops the packer, closing the channel.
            pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected => quit.quit(),
            _ => {}
        })
        .param_changed(|_, (_, info), id, param| {
            if let Some(param) = param
                && id == spa::param::ParamType::Format.as_raw()
            {
                let _ = info.parse(param);
            }
        })
        .process(|stream, (packer, info)| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(d) = buffer.datas_mut().first_mut() else { return };
            let (offset, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
            let ch = info.channels() as usize;
            let Some(data) = d.data() else { return };
            let Some(bytes) = data.get(offset..offset + size) else { return };
            if ch == 0 {
                return;
            }
            let sample = |f: &[u8], i: usize| f32::from_le_bytes(f[4 * i..4 * i + 4].try_into().unwrap());
            packer.push(info.rate(), bytes.chunks_exact(4 * ch).map(|f| stereo(ch, |i| sample(f, i))));
        })
        .register()?;

    let mut want = AudioInfoRaw::new();
    want.set_format(AudioFormat::F32LE);
    want.set_rate(RATE);
    want.set_channels(2);
    let mut position = [0; MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    want.set_position(position);
    let format = Value::Object(Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: want.into(),
    });
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &format)?.0.into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("bad audio format pod")?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    mainloop.run();
    Ok(())
}

/// Device sound in, Opus packets out: 48 kHz stereo, 20 ms per packet.
struct Packer {
    opus: Encoder,
    resample: Resampler,
    /// The input rate `resample` was made for.
    rate: u32,
    /// 48 kHz stereo, interleaved, until a 20 ms frame is full.
    pcm: Vec<f32>,
    /// Silent frames in a row.
    quiet: usize,
    tx: mpsc::Sender<Vec<u8>>,
}

impl Packer {
    fn new(tx: mpsc::Sender<Vec<u8>>) -> anyhow::Result<Self> {
        let resample = Resampler::new(RATE, RATE);
        Ok(Self { opus: Encoder::new()?, resample, rate: RATE, pcm: Vec::with_capacity(4 * FRAME), quiet: 0, tx })
    }

    fn push(&mut self, rate: u32, frames: impl Iterator<Item = [f32; 2]>) {
        if rate == 0 {
            return;
        }
        if rate != self.rate {
            (self.resample, self.rate) = (Resampler::new(rate, RATE), rate);
        }
        for f in frames {
            let pcm = &mut self.pcm;
            self.resample.push(f, |y| pcm.extend_from_slice(&y));
            if self.pcm.len() >= 2 * FRAME {
                self.send();
            }
        }
    }

    /// Encode and send the first 20 ms, unless it's been silent for a while. Sound after
    /// silence goes out with its very first frame.
    fn send(&mut self) {
        let frame = &self.pcm[..2 * FRAME];
        self.quiet = if frame.iter().all(|s| s.abs() < SILENCE) { self.quiet + 1 } else { 0 };
        if self.quiet <= QUIET_FRAMES
            && let Some(p) = self.opus.encode(frame)
        {
            let _ = self.tx.try_send(p);
        }
        self.pcm.drain(..2 * FRAME);
    }
}

/// One device frame of `n` channels as stereo. 5.1 and 7.1 keep the center (dialogue).
fn stereo(n: usize, s: impl Fn(usize) -> f32) -> [f32; 2] {
    match n {
        0 | 1 => [s(0); 2],
        2..6 => [s(0), s(1)],
        _ => {
            let c = 0.7 * s(2);
            [s(0) + c, s(1) + c]
        }
    }
}

/// Linear interpolation between neighbouring stereo frames. No low-pass filter: fine for the
/// usual 44.1 and 48 kHz, a little aliasing when a device runs at 96 kHz or more.
struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Where the next output frame falls after `prev`, in input frames.
    t: f64,
    prev: [f32; 2],
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self { step: from as f64 / to.max(1) as f64, t: 0.0, prev: [0.0; 2] }
    }

    /// Feed one input frame; `out` gets every output frame that falls before it.
    fn push(&mut self, x: [f32; 2], mut out: impl FnMut([f32; 2])) {
        while self.t < 1.0 {
            let t = self.t as f32;
            out([self.prev[0] + (x[0] - self.prev[0]) * t, self.prev[1] + (x[1] - self.prev[1]) * t]);
            self.t += self.step;
        }
        self.t -= 1.0;
        self.prev = x;
    }
}

// ---------------------------------------------------------------- playback

/// Decoded sound waiting for the speakers, at the device's rate. Shared by `Player::push` and
/// the device callback; both hold the lock only for a few microseconds.
struct Jitter {
    opus: Decoder,
    resample: Resampler,
    queue: VecDeque<[f32; 2]>,
    /// Off until `start` frames are queued, so a little network jitter doesn't stutter.
    playing: bool,
    /// Missing packets concealed in a row.
    concealed: u32,
    start: usize,
    max: usize,
    pcm: Vec<f32>,
}

impl Jitter {
    fn new(rate: u32) -> anyhow::Result<Self> {
        let ms = |n: u32| (rate as u64 * n as u64 / 1000) as usize;
        Ok(Self {
            opus: Decoder::new()?,
            resample: Resampler::new(RATE, rate),
            queue: VecDeque::new(),
            playing: false,
            concealed: 0,
            start: ms(40),
            max: ms(150),
            pcm: vec![0.0; 2 * MAX_FRAME],
        })
    }

    fn packet(&mut self, packet: &[u8]) {
        if !packet.is_empty() {
            self.concealed = 0;
            self.decode(Some(packet));
        }
    }

    /// Decode a packet, or conceal a missing one (`None`), onto the queue. Past ~150 ms the
    /// oldest sound goes: better a skip than a growing delay.
    fn decode(&mut self, packet: Option<&[u8]>) {
        let n = self.opus.decode(packet, &mut self.pcm);
        let queue = &mut self.queue;
        for f in self.pcm[..2 * n].chunks_exact(2) {
            self.resample.push([f[0], f[1]], |y| queue.push_back(y));
        }
        let excess = self.queue.len().saturating_sub(self.max);
        self.queue.drain(..excess);
        self.playing |= self.queue.len() >= self.start;
    }

    /// The next frame for the speakers. A late or lost packet is concealed by Opus for up to
    /// 60 ms; after that it's silence until 40 ms are buffered again.
    fn next(&mut self) -> [f32; 2] {
        if !self.playing {
            return [0.0; 2];
        }
        if self.queue.is_empty() && self.concealed < 3 {
            self.concealed += 1;
            self.decode(None);
        }
        self.queue.pop_front().unwrap_or_else(|| {
            self.playing = false;
            [0.0; 2]
        })
    }
}

fn output<T>(device: &cpal::Device, config: cpal::StreamConfig, jitter: Arc<Mutex<Jitter>>) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = config.channels.max(1) as usize;
    device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut j = jitter.lock().unwrap_or_else(|e| e.into_inner());
            for f in out.chunks_mut(ch) {
                let [l, r] = j.next();
                for (i, x) in f.iter_mut().enumerate() {
                    // Mono gets both sides; channels past the first two stay silent.
                    let v = match (ch, i) {
                        (1, _) => (l + r) / 2.0,
                        (_, 0) => l,
                        (_, 1) => r,
                        _ => 0.0,
                    };
                    *x = v.clamp(-1.0, 1.0).to_sample();
                }
            }
        },
        |_| {},
        None,
    )
}

// ---------------------------------------------------------------- Opus

/// libopus's raw-pointer API, wrapped once.
struct Encoder(*mut opus::OpusEncoder);

// Plain memory owned by this value; libopus keeps no thread state.
unsafe impl Send for Encoder {}

impl Encoder {
    fn new() -> anyhow::Result<Self> {
        let mut err = 0;
        let e = unsafe { opus::opus_encoder_create(RATE as i32, 2, opus::OPUS_APPLICATION_AUDIO, &mut err) };
        anyhow::ensure!(!e.is_null(), "the sound encoder didn't start (Opus error {err})");
        let e = Self(e);
        let ok = unsafe { opus::opus_encoder_ctl!(e.0, opus::OPUS_SET_BITRATE_REQUEST, BITRATE) };
        anyhow::ensure!(ok == opus::OPUS_OK, "the sound encoder refused its bitrate (Opus error {ok})");
        Ok(e)
    }

    /// One 20 ms frame of interleaved stereo to one packet.
    fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        assert_eq!(pcm.len(), 2 * FRAME);
        let mut out = vec![0; MAX_PACKET];
        let n = unsafe { opus::opus_encode_float(self.0, pcm.as_ptr(), FRAME as i32, out.as_mut_ptr(), MAX_PACKET as i32) };
        if n <= 0 {
            return None;
        }
        out.truncate(n as usize);
        Some(out)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus::opus_encoder_destroy(self.0) }
    }
}

struct Decoder(*mut opus::OpusDecoder);

// Plain memory owned by this value; libopus keeps no thread state.
unsafe impl Send for Decoder {}

impl Decoder {
    fn new() -> anyhow::Result<Self> {
        let mut err = 0;
        let d = unsafe { opus::opus_decoder_create(RATE as i32, 2, &mut err) };
        anyhow::ensure!(!d.is_null(), "the sound decoder didn't start (Opus error {err})");
        Ok(Self(d))
    }

    /// Decode a packet, or conceal a lost one (`None`), into interleaved stereo `pcm`. Returns
    /// samples per channel: 0 for anything that isn't valid Opus.
    fn decode(&mut self, packet: Option<&[u8]>, pcm: &mut [f32]) -> usize {
        let (data, len, max) = match packet {
            Some(p) => (p.as_ptr(), p.len().min(i32::MAX as usize) as i32, pcm.len() / 2),
            // Concealment makes exactly the frame that's missing.
            None => (std::ptr::null(), 0, FRAME.min(pcm.len() / 2)),
        };
        let n = unsafe { opus::opus_decode_float(self.0, data, len, pcm.as_mut_ptr(), max as i32, 0) };
        n.max(0) as usize
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus::opus_decoder_destroy(self.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<[f32; 2]> {
        let n = (rate as f32 * secs) as usize;
        (0..n).map(|i| [0.5 * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin(); 2]).collect()
    }

    /// (frequency from zero crossings, RMS) of mono 48 kHz samples.
    fn measure(s: &[f32]) -> (f32, f32) {
        let crossings = s.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
        (crossings as f32 / 2.0 / (s.len() as f32 / RATE as f32), rms)
    }

    fn drain(rx: &mut mpsc::Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// Left channel of every packet, decoded.
    fn decode_all(packets: &[Vec<u8>]) -> Vec<f32> {
        let (mut dec, mut pcm, mut out) = (Decoder::new().unwrap(), vec![0.0; 2 * MAX_FRAME], Vec::new());
        for p in packets {
            let n = dec.decode(Some(p), &mut pcm);
            assert_eq!(n, FRAME, "every packet is 20 ms");
            out.extend(pcm[..2 * n].chunks_exact(2).map(|f| f[0]));
        }
        out
    }

    #[test]
    fn round_trip_keeps_the_tone() {
        let (tx, mut rx) = mpsc::channel(1000);
        let mut packer = Packer::new(tx).unwrap();
        // 44.1 kHz in, so the resampler is in the path too.
        let started = Instant::now();
        packer.push(44_100, tone(44_100, 440.0, 1.0).into_iter());
        let took = started.elapsed();
        let packets = drain(&mut rx);
        assert!((49..=50).contains(&packets.len()), "1 s is 50 packets, got {}", packets.len());
        assert!(packets.iter().all(|p| p.len() <= MAX_PACKET));
        let kbps = packets.iter().map(Vec::len).sum::<usize>() * 8 / (packets.len() * 20);
        let (hz, rms) = measure(&decode_all(&packets)[4800..]);
        println!("{kbps} kbit/s, {hz:.1} Hz, rms {rms:.3}, {:.2} ms per frame", took.as_secs_f64() * 1000.0 / 50.0);
        assert!((40..=90).contains(&kbps), "{kbps} kbit/s");
        assert!((hz - 440.0).abs() < 5.0, "{hz} Hz");
        assert!((rms - 0.354).abs() < 0.05, "rms {rms}");
    }

    #[test]
    fn silence_stops_sending_and_sound_resumes_at_once() {
        let (tx, mut rx) = mpsc::channel(1000);
        let mut packer = Packer::new(tx).unwrap();
        packer.push(RATE, std::iter::repeat_n([0.0; 2], 2 * RATE as usize));
        assert_eq!(drain(&mut rx).len(), QUIET_FRAMES);
        packer.push(RATE, tone(RATE, 440.0, 0.02).into_iter());
        assert_eq!(drain(&mut rx).len(), 1, "the first 20 ms of sound goes out right away");
    }

    #[test]
    fn jitter_buffer_waits_caps_and_conceals() {
        let mut enc = Encoder::new().unwrap();
        let pcm: Vec<f32> = tone(RATE, 440.0, 0.02).into_iter().flatten().collect();
        let packet = enc.encode(&pcm).unwrap();
        let mut j = Jitter::new(RATE).unwrap();

        j.packet(&packet);
        assert_eq!(j.next(), [0.0; 2], "waits for 40 ms");
        j.packet(&packet);
        assert!(j.playing);
        for _ in 0..20 {
            j.packet(&packet);
        }
        assert_eq!(j.queue.len(), 7200, "capped at 150 ms");

        // Played out, then packets stop: 60 ms concealed, then silence until refilled.
        for _ in 0..7200 + 3 * FRAME {
            j.next();
        }
        assert!(j.playing && j.queue.is_empty() && j.concealed == 3);
        assert_eq!(j.next(), [0.0; 2]);
        assert!(!j.playing);
        j.packet(&packet);
        assert!(!j.playing && j.concealed == 0);
        j.packet(&packet);
        assert!(j.playing);

        // Garbage from the network is skipped.
        j.packet(&[0xff; 7]);
        j.packet(&[]);
    }

    /// Plays a tone through `Player` while `Capture` records the speakers (audible). Run with
    /// `cargo test loopback -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn loopback_hears_the_player() {
        let mut player = Player::new().unwrap();
        let (capture, mut rx) = Capture::start().unwrap();
        let got = std::thread::spawn(move || std::iter::from_fn(|| rx.blocking_recv()).collect::<Vec<_>>());

        let mut enc = Encoder::new().unwrap();
        let pcm: Vec<f32> = tone(RATE, 660.0, 1.5).into_iter().flatten().collect();
        let started = Instant::now();
        for (k, frame) in pcm.chunks_exact(2 * FRAME).enumerate() {
            player.push(&enc.encode(frame).unwrap());
            std::thread::sleep((started + Duration::from_millis(20 * k as u64 + 20)).saturating_duration_since(Instant::now()));
        }
        std::thread::sleep(Duration::from_millis(200));
        drop(capture);
        let packets = got.join().unwrap();
        let sound = decode_all(&packets);
        let (hz, rms) = measure(&sound[sound.len().min(4800)..]);
        println!("{} packets, {hz:.1} Hz, rms {rms:.3}", packets.len());
        assert!(packets.len() >= 25, "expected at least 0.5 s of packets, got {}", packets.len());
        assert!(rms > 0.01, "captured near silence (rms {rms})");
    }
}
