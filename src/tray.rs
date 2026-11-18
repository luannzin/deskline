//! System tray icon (STUB: to be implemented): Deskline keeps accepting sessions with its window
//! closed.
#![allow(dead_code)]
use std::sync::mpsc::Receiver;

use eframe::egui::Context;

pub enum TrayMsg {
    Open,
    Quit,
}

pub struct Tray {
    pub rx: Receiver<TrayMsg>,
}

impl Tray {
    /// None when the desktop has no tray.
    pub fn start(_ctx: &Context) -> Option<Tray> {
        None
    }

    pub fn tooltip(&mut self, _tip: &str) {}

    /// Badge the icon while someone waits for an answer.
    pub fn alert(&mut self, _on: bool) {}
}
