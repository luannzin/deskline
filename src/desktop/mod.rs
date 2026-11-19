//! The shared screen: captured frames out, injected input in. One backend per OS.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::Key;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

#[cfg_attr(all(target_os = "linux", not(test)), path = "linux.rs")]
#[cfg_attr(all(windows, not(test)), path = "windows.rs")]
#[cfg_attr(all(target_os = "macos", not(test)), path = "macos.rs")]
#[cfg_attr(test, path = "fake.rs")]
pub(crate) mod imp;

/// Tests use the fake screen, so the Linux backend's own unit tests need it compiled beside it.
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "linux.rs"]
mod linux;

#[cfg(windows)]
#[path = "gpu.rs"]
mod gpu;
#[cfg(windows)]
pub use gpu::Gpu;

/// Pictures stay on the GPU only on Windows.
#[cfg(not(windows))]
pub enum Gpu {}

/// One captured frame, 4 bytes per pixel, rows packed with no padding.
pub struct Frame {
    pub w: usize,
    pub h: usize,
    /// Byte order BGRA (or BGRx) when true, RGBA (or RGBx) when false.
    pub bgra: bool,
    /// Empty when the picture is only on the GPU (`gpu`).
    pub px: Vec<u8>,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub gpu: Option<Gpu>,
}

/// One screen the host can share.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Monitor {
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub primary: bool,
}

/// Input from the viewer. Positions are 0..1 across the shared screen; keys are Linux evdev
/// codes, which every backend translates.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Move { x: f32, y: f32 },
    /// 0 left, 1 right, 2 middle, 3 back, 4 forward.
    Button { button: u8, down: bool },
    /// Wheel notches. `y` positive scrolls up (away from the user), `x` positive scrolls right.
    Scroll { x: i32, y: i32 },
    Key { code: u16, down: bool },
    /// The secure attention sequence: Windows' sign-in and lock screen answer only to this.
    CtrlAltDel,
    /// The pointer moved this many counts, wherever it is: games that hide the pointer and read
    /// raw movement (the viewer captured its mouse). New in 0.5; older hosts skip it.
    MoveBy { dx: i32, dy: i32 },
}

type Tx = watch::Sender<Option<Arc<Frame>>>;
type Rx = watch::Receiver<Option<Arc<Frame>>>;

pub struct Screen {
    imp: imp::Screen,
    /// Keys and buttons the viewer holds down, released when control ends so nothing sticks.
    held: Vec<Input>,
    /// Stays the same channel when `swap` puts a new capture behind it, so sessions go on.
    pub frames: Rx,
    relay: Relay,
}

/// A capture `Screen::prepare` opened, not showing yet.
pub struct Prepared(imp::Screen, Rx);

impl Prepared {
    pub async fn close(self) {
        self.0.close().await;
    }
}

impl Screen {
    /// Start capturing the primary monitor. On Linux this may show the system's sharing dialog.
    pub async fn open(home: &Path) -> anyhow::Result<Self> {
        let Prepared(imp, inner) = Self::prepare(home, false).await?;
        let (tx, frames) = watch::channel(None);
        Ok(Self { imp, held: Vec::new(), frames, relay: Relay::start(inner, tx) })
    }

    /// Open a second capture beside this one. `fresh`: ask the system again rather than reuse the
    /// answer it remembered (Linux portals: which screens, and whether input is allowed). Waits
    /// for that dialog as long as the person takes, so call it without holding the `Screen`.
    pub async fn prepare(home: &Path, fresh: bool) -> anyhow::Result<Prepared> {
        let (imp, frames) = imp::Screen::open(home, fresh).await?;
        Ok(Prepared(imp, frames))
    }

    /// Show `p` from now on, on the same `frames`; the old capture closes. Monitors and whether
    /// input works may have changed.
    pub async fn swap(&mut self, p: Prepared) -> anyhow::Result<()> {
        let Prepared(imp, inner) = p;
        let Some(tx) = self.relay.stop().await else {
            imp.close().await;
            anyhow::bail!("screen capture has stopped");
        };
        self.release().await;
        std::mem::replace(&mut self.imp, imp).close().await;
        self.relay = Relay::start(inner, tx);
        Ok(())
    }

    /// The system lets this capture drive the mouse and keyboard. False where someone turned
    /// that off in the system's sharing dialog (Linux portals): `prepare(fresh)` asks again.
    pub fn input_allowed(&self) -> bool {
        self.imp.input_allowed()
    }

    /// The most frames a second to capture: the fastest any viewer's picture wants.
    pub fn set_fps(&mut self, fps: u32) -> anyhow::Result<()> {
        self.imp.set_fps(fps.clamp(1, 240))
    }

    /// The screens that can be shared, in the order `select` takes them.
    pub fn monitors(&self) -> Vec<Monitor> {
        self.imp.monitors()
    }

    /// Which of `monitors()` is being shared.
    pub fn current(&self) -> usize {
        self.imp.current()
    }

    /// Share another monitor. Frames keep arriving on the same `frames` channel.
    pub async fn select(&mut self, index: usize) -> anyhow::Result<()> {
        anyhow::ensure!(index < self.monitors().len(), "There's no screen {} on this computer.", index + 1);
        self.release().await;
        self.imp.select(index).await
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        let (held, down) = match i {
            Input::Key { code, down } => (Input::Key { code, down: true }, down),
            Input::Button { button, down } => (Input::Button { button, down: true }, down),
            _ => return self.imp.input(i).await,
        };
        // Wayland apps repeat a held key themselves; a second press would double it.
        if down && cfg!(target_os = "linux") && matches!(i, Input::Key { .. }) && self.held.contains(&held) {
            return Ok(());
        }
        self.held.retain(|h| *h != held);
        if down {
            self.held.push(held);
        }
        self.imp.input(i).await
    }

    /// Privacy mode: blank this computer's own display and ignore its own mouse and keyboard
    /// while the viewer works. Errors where the system can't do it.
    pub async fn privacy(&mut self, on: bool) -> anyhow::Result<()> {
        self.imp.privacy(on).await
    }

    pub async fn release(&mut self) {
        for i in std::mem::take(&mut self.held) {
            let up = match i {
                Input::Key { code, .. } => Input::Key { code, down: false },
                Input::Button { button, .. } => Input::Button { button, down: false },
                other => other,
            };
            let _ = self.imp.input(up).await;
        }
    }

    pub async fn close(mut self) {
        self.release().await;
        let _ = self.imp.privacy(false).await;
        self.imp.close().await;
    }
}

/// Passes a capture's frames on to `Screen::frames`. Stopped, it hands the sender back for the
/// next capture; if the capture ends on its own (the system's "stop sharing"), the sender drops,
/// which ends the sessions.
struct Relay {
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Option<Tx>>,
}

impl Relay {
    fn start(mut inner: Rx, tx: Tx) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    r = &mut stopped => return r.is_ok().then_some(tx),
                    r = inner.changed() => match r {
                        Ok(()) => {
                            tx.send_replace(inner.borrow_and_update().clone());
                        }
                        Err(_) => return None,
                    },
                }
            }
        });
        Self { stop: Some(stop), task }
    }

    async fn stop(&mut self) -> Option<Tx> {
        let _ = self.stop.take()?.send(());
        (&mut self.task).await.ok().flatten()
    }
}

/// Frame buffers to reuse. A 4K frame is 33 MB: a new one per frame is fresh memory from the
/// system every time (malloc maps anything over 32 MB, Windows anything over 512 KB), and
/// faulting it in costs milliseconds a frame.
// ponytail: macOS still allocates per frame (its copy is in a CoreGraphics callback); add it
// there when 4K capture on a Mac shows up in a profile.
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub struct Pool(Vec<Arc<Frame>>);

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Pool {
    /// A frame nobody holds any more, to fill again: its buffer, or its GPU texture.
    pub fn reuse(&mut self) -> Option<Frame> {
        let free = self.0.iter().position(|f| Arc::strong_count(f) == 1)?;
        Arc::into_inner(self.0.swap_remove(free))
    }

    /// An empty buffer with room for `len` bytes, reused if there is one.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn take(&mut self, len: usize) -> Vec<u8> {
        let mut px = self.reuse().map_or_else(Vec::new, |f| f.px);
        px.clear();
        px.reserve(len);
        px
    }

    /// Publish `f`, keeping the frame it replaces for reuse.
    pub fn send(&mut self, tx: &Tx, f: Frame) {
        if let Some(old) = tx.send_replace(Some(Arc::new(f))) {
            self.keep(old);
        }
    }

    /// A frame that wasn't sent (nothing changed): its buffer can be used again.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn recycle(&mut self, f: Frame) {
        self.keep(Arc::new(f));
    }

    fn keep(&mut self, f: Arc<Frame>) {
        // Three is enough: the channel, the encoder and the frame in between let go in turn.
        if self.0.len() < 3 {
            self.0.push(f);
        }
    }
}

/// Frames a second captured until a viewer wants more: plenty for a desktop.
pub const DEFAULT_FPS: u32 = 30;

/// The pause between captures at `fps` (0: `DEFAULT_FPS`).
#[cfg_attr(any(target_os = "macos", test), allow(dead_code))]
pub fn gap(fps: u32) -> Duration {
    Duration::from_secs(1) / if fps == 0 { DEFAULT_FPS } else { fps }
}

pub const SHIFT: u16 = 42;
pub const CTRL: u16 = 29;
pub const ALT: u16 = 56;

/// egui key (by physical position) to Linux evdev code.
pub fn evdev(key: Key) -> Option<u16> {
    use Key::*;
    Some(match key {
        Escape => 1,
        Num1 => 2,
        Num2 => 3,
        Num3 => 4,
        Num4 => 5,
        Num5 => 6,
        Num6 => 7,
        Num7 => 8,
        Num8 => 9,
        Num9 => 10,
        Num0 => 11,
        Minus => 12,
        Equals => 13,
        Backspace => 14,
        Tab => 15,
        Q => 16,
        W => 17,
        E => 18,
        R => 19,
        T => 20,
        Y => 21,
        U => 22,
        I => 23,
        O => 24,
        P => 25,
        OpenBracket => 26,
        CloseBracket => 27,
        Enter => 28,
        A => 30,
        S => 31,
        D => 32,
        F => 33,
        G => 34,
        H => 35,
        J => 36,
        K => 37,
        L => 38,
        Semicolon => 39,
        Quote => 40,
        Backtick => 41,
        Backslash => 43,
        Z => 44,
        X => 45,
        C => 46,
        V => 47,
        B => 48,
        N => 49,
        M => 50,
        Comma => 51,
        Period => 52,
        Slash => 53,
        Space => 57,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        Plus => 78,
        F11 => 87,
        F12 => 88,
        Home => 102,
        ArrowUp => 103,
        PageUp => 104,
        ArrowLeft => 105,
        ArrowRight => 106,
        End => 107,
        ArrowDown => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        BrowserBack => 158,
        F13 => 183,
        F14 => 184,
        F15 => 185,
        F16 => 186,
        F17 => 187,
        F18 => 188,
        F19 => 189,
        F20 => 190,
        F21 => 191,
        F22 => 192,
        F23 => 193,
        F24 => 194,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    #[tokio::test(flavor = "multi_thread")]
    async fn swapped_capture_keeps_the_channel_and_closing_ends_it() {
        let wait = Duration::from_secs(2);
        let home = std::env::temp_dir();
        let mut s = Screen::open(&home).await.unwrap();
        let mut frames = s.frames.clone();
        timeout(wait, frames.changed()).await.unwrap().unwrap();
        s.swap(Screen::prepare(&home, true).await.unwrap()).await.unwrap();
        for _ in 0..3 {
            timeout(wait, frames.changed()).await.expect("frames stopped after the swap").unwrap();
        }
        s.close().await;
        let closed = timeout(wait, async {
            while frames.changed().await.is_ok() {}
        });
        closed.await.expect("closing the screen must end its frames");
    }

    #[test]
    fn pool_hands_back_only_buffers_nobody_holds() {
        let (tx, rx) = watch::channel(None);
        let mut pool = Pool::default();
        let frame = |px: Vec<u8>| Frame { w: 2, h: 2, bgra: true, px, gpu: None };
        let mut px = pool.take(16);
        px.extend_from_slice(&[1; 16]);
        let first = px.as_ptr();
        pool.send(&tx, frame(px));
        pool.send(&tx, frame(vec![2; 16]));
        // The first frame was replaced and nothing holds it: its buffer comes back, emptied.
        let again = pool.take(16);
        assert_eq!((again.as_ptr(), again.len()), (first, 0));
        // The second is held (an encoder still working on it) once replaced: not handed out.
        let held = rx.borrow().clone().unwrap();
        pool.send(&tx, frame(again));
        let fresh = pool.take(16);
        assert_ne!(fresh.as_ptr(), held.px.as_ptr());
    }
}
