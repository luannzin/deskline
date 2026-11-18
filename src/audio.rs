//! The host's sound to the viewer (STUB: to be implemented). Capture what the host's speakers
//! play, encode 20 ms Opus packets; the viewer decodes and plays them.
#![allow(dead_code)]
use tokio::sync::mpsc;

/// Capturing the default output device's sound. Dropping it stops.
pub struct Capture;

impl Capture {
    /// Packets: one Opus frame each (20 ms, 48 kHz stereo), small enough for one QUIC datagram.
    pub fn start() -> anyhow::Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        anyhow::bail!("not implemented")
    }
}

/// Plays packets from `Capture` on the default output device. Dropping it stops.
pub struct Player;

impl Player {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("not implemented")
    }

    /// Queue one packet. Late or lost packets are fine; the queue never grows past ~150 ms.
    pub fn push(&mut self, _packet: &[u8]) {}
}
