//! Windows viewer: a low-level keyboard hook (STUB: to be implemented). While grabbing and
//! Deskline's window has focus, every key (Win, Alt+Tab, Print Screen, numpad, media keys…)
//! goes to the remote screen instead of this computer.
#![allow(dead_code)]
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::Context;

/// Install the hook on its own thread. Keys arrive as (Linux evdev code, down).
pub fn start(_ctx: Context) -> Receiver<(u16, bool)> {
    channel().1
}

/// Grab keys (while Deskline's window has focus) or let them through.
pub fn grab(_on: bool) {}

/// Whether the hook is installed and grabbing right now.
pub fn grabbing() -> bool {
    false
}
