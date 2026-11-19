//! The wire: what the viewer and the host say to each other, and how.
//!
//! One QUIC connection per session. The viewer opens the control stream (bidirectional): a
//! `Hello`, then `Ask`s one way and `Tell`s the other. The host opens one unidirectional stream
//! for video. Sound travels in QUIC datagrams, since late sound is worse than lost sound. Every
//! later bidirectional stream the viewer opens is one file operation (files.rs).
//!
//! Messages are postcard, each prefixed with its length. A message that doesn't decode is
//! skipped, so a newer version can add `Ask`/`Tell` variants at the END and older versions
//! still talk to it. `Hello` must never change shape. The same framing carries file transfers
//! and the Windows service's pipe.
use iroh::endpoint::RecvStream;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::codec::Quality;
use crate::desktop::{Input, Monitor};

pub const ALPN: &[u8] = b"deskline/2";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Close code: the host ended the session on purpose, so the viewer shouldn't reconnect.
pub const STOPPED: u32 = 1;
/// QUIC sends higher priorities first: the control stream (acks, input) before video, video
/// before file streams (0).
pub const CONTROL_PRIORITY: i32 = 2;
pub const VIDEO_PRIORITY: i32 = 1;

/// The viewer's first message.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Hello {
    pub version: String,
    pub name: String,
    /// The host's unattended-access password, when the viewer has it.
    pub password: Option<String>,
    /// With the password: let me in without it from now on.
    pub remember: bool,
    /// Back after the link dropped: the host lets the same viewer straight back in for a while.
    pub resume: bool,
}

/// Viewer to host.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Ask {
    WantControl,
    Input(Input),
    Ack(u32),
    Keyframe,
    Bye,
    /// Share monitor n of `Tell::Monitors`.
    Monitor(usize),
    Clip(String),
    Chat(String),
    /// Send me the host's sound, or stop.
    Sound(bool),
    /// Blank the host's own screen and lock its own mouse and keyboard, or undo that.
    Privacy(bool),
    Quality(Quality),
    /// Ask the host's system again which screens to share and whether input is allowed (its
    /// sharing dialog, on Linux). New in 0.5.
    Reshare,
}

/// Host to viewer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Tell {
    Accepted { name: String, control: bool },
    /// `password`: the host has unattended access, so the password would get you in.
    Denied { why: String, password: bool },
    Control(bool),
    Monitors { list: Vec<Monitor>, current: usize },
    Clip(String),
    Chat(String),
    Privacy(bool),
    /// Something to show the viewer ("Privacy mode isn't available on Linux.").
    Note(String),
}

#[derive(Serialize, Deserialize)]
pub struct Packet {
    pub seq: u32,
    pub data: Vec<u8>,
}

/// The biggest message either side sends or takes.
const MAX: usize = 16 << 20;

pub async fn send_msg<T: Serialize, W: AsyncWrite + Unpin>(w: &mut W, m: &T) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(m)?;
    anyhow::ensure!(bytes.len() <= MAX, "That's too much to send at once.");
    w.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    Ok(())
}

async fn recv_raw<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(n <= MAX, "message too large: {n} bytes");
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn recv_msg<T: DeserializeOwned, R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<T> {
    Ok(postcard::from_bytes(&recv_raw(r).await?)?)
}

/// Read messages on their own task (`recv_msg` must never be cancelled halfway through one).
/// Messages this version doesn't know are skipped; the channel closes with the stream.
pub fn reader<T: DeserializeOwned + Send + 'static>(mut r: RecvStream) -> mpsc::UnboundedReceiver<T> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(raw) = recv_raw(&mut r).await {
            let Ok(m) = postcard::from_bytes(&raw) else { continue };
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    rx
}

/// Names come from the other side: keep them short and printable.
pub fn clean(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(40).collect();
    if s.trim().is_empty() { "Someone".into() } else { s }
}

/// Chat lines come from the other side too: printable, at most 2000 characters.
pub fn clean_chat(s: &str) -> String {
    s.chars().filter(|c| *c == '\n' || !c.is_control()).take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_messages_are_skippable_and_known_ones_round_trip() {
        let a = Ask::Input(Input::Scroll { x: 1, y: -2 });
        assert_eq!(postcard::from_bytes::<Ask>(&postcard::to_stdvec(&a).unwrap()).unwrap(), a);
        // A variant index from the future doesn't decode; `reader` skips it.
        assert!(postcard::from_bytes::<Ask>(&[200, 1]).is_err());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean("  "), "Someone");
        assert_eq!(clean("a\u{7}b"), "ab");
        assert_eq!(clean(&"x".repeat(100)).len(), 40);
    }
}
