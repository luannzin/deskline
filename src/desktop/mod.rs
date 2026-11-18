//! The shared screen: captured frames out, injected input in. One backend per OS.
use std::path::Path;
use std::sync::Arc;

use eframe::egui::Key;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[cfg_attr(all(target_os = "linux", not(test)), path = "linux.rs")]
#[cfg_attr(all(windows, not(test)), path = "windows.rs")]
#[cfg_attr(all(target_os = "macos", not(test)), path = "macos.rs")]
#[cfg_attr(test, path = "fake.rs")]
pub(crate) mod imp;

/// One captured frame, 4 bytes per pixel, rows packed with no padding.
pub struct Frame {
    pub w: usize,
    pub h: usize,
    /// Byte order BGRA (or BGRx) when true, RGBA (or RGBx) when false.
    pub bgra: bool,
    pub px: Vec<u8>,
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
}

pub struct Screen {
    imp: imp::Screen,
    /// Keys and buttons the viewer holds down, released when control ends so nothing sticks.
    held: Vec<Input>,
    pub frames: watch::Receiver<Option<Arc<Frame>>>,
}

impl Screen {
    /// Start capturing the primary monitor. On Linux this may show the system's sharing dialog.
    pub async fn open(home: &Path) -> anyhow::Result<Self> {
        let (imp, frames) = imp::Screen::open(home).await?;
        Ok(Self { imp, held: Vec::new(), frames })
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
        self.release().await;
        self.imp.select(index).await
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        match i {
            Input::Key { code, down } => {
                let held = Input::Key { code, down: true };
                // Wayland apps repeat a held key themselves; a second press would double it.
                if down && cfg!(target_os = "linux") && self.held.contains(&held) {
                    return Ok(());
                }
                self.held.retain(|h| *h != held);
                if down {
                    self.held.push(i);
                }
            }
            Input::Button { button, down } => {
                self.held.retain(|h| *h != Input::Button { button, down: true });
                if down {
                    self.held.push(i);
                }
            }
            _ => {}
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
