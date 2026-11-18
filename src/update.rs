//! Updates from GitHub Releases (STUB: to be implemented).
#![allow(dead_code)]
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

pub enum Msg {
    /// A newer version is downloaded: (release tag, file).
    Ready(String, PathBuf),
    /// Installed; time to quit (the new version starts on its own).
    Installed,
    Failed(String),
}

pub struct Updater {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub ready: Option<(String, PathBuf)>,
    pub installing: bool,
}

impl Updater {
    /// Checks now and every few hours in the background; downloads into `dir`.
    pub fn start(_dir: PathBuf) -> Updater {
        let (tx, rx) = channel();
        Updater { tx, rx, ready: None, installing: false }
    }

    /// Install `ready` off the UI thread; `Installed` or `Failed` follows.
    pub fn install(&mut self) {}
}

/// The installed Windows service's `--server`: update silently and restart the service.
pub fn run_in_service(_dir: PathBuf) {}

/// Start the newly installed version after `Installed`.
pub fn relaunch() {}
