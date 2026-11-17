//! The shared screen: captured frames out, injected input in. One backend per OS.
use std::path::Path;
use std::sync::Arc;

use eframe::egui::Key;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[cfg_attr(all(target_os = "linux", not(test)), path = "linux.rs")]
#[cfg_attr(all(windows, not(test)), path = "windows.rs")]
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

/// Input from the viewer. Positions are 0..1 across the shared screen; keys are Linux evdev
/// codes, which both backends understand.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Move { x: f32, y: f32 },
    /// 0 left, 1 right, 2 middle.
    Button { button: u8, down: bool },
    /// Wheel notches, positive is away from the user (scroll up).
    Scroll { steps: i32 },
    Key { code: u16, down: bool },
}

pub struct Screen {
    imp: imp::Screen,
    /// Keys and buttons the viewer holds down, released when control ends so nothing sticks.
    held: Vec<Input>,
    pub frames: watch::Receiver<Option<Arc<Frame>>>,
}

impl Screen {
    /// Start capturing. On Linux this shows the system's screen sharing dialog.
    pub async fn open(home: &Path) -> anyhow::Result<Self> {
        let (imp, frames) = imp::Screen::open(home).await?;
        Ok(Self { imp, held: Vec::new(), frames })
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
        _ => return None,
    })
}
