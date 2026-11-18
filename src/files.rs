//! File transfer (STUB: to be implemented). The viewer opens one extra QUIC bidirectional stream
//! per operation: list a folder on the host, upload a file or folder into the host's Downloads,
//! or download a file or folder from the host. The host serves each stream with `serve`.
#![allow(dead_code)]
use std::path::{Path, PathBuf};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Listing {
    /// The folder listed, as the host spells it. Empty asks for the starting places (home,
    /// Desktop, Downloads, drives).
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

/// Bytes done, bytes total.
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// Viewer: what's in folder `path` on the host.
pub async fn list(_conn: &Connection, _path: &str) -> anyhow::Result<Listing> {
    anyhow::bail!("not implemented")
}

/// Viewer: send a local file or folder into the host's Downloads. Returns where it landed.
pub async fn upload(_conn: &Connection, _local: &Path, _progress: Progress<'_>) -> anyhow::Result<String> {
    anyhow::bail!("not implemented")
}

/// Viewer: fetch a file or folder from the host into `dest_dir`. Returns the local path.
pub async fn download(_conn: &Connection, _remote: &str, _dest_dir: &Path, _progress: Progress<'_>) -> anyhow::Result<PathBuf> {
    anyhow::bail!("not implemented")
}

/// Host: answer one stream the viewer opened. Returns a line for the session log, if any
/// ("Received report.pdf", "Sent Photos").
pub async fn serve(_send: SendStream, _recv: RecvStream) -> anyhow::Result<Option<String>> {
    anyhow::bail!("not implemented")
}

/// This user's Downloads folder (on the host: the signed-in user's).
pub fn downloads() -> anyhow::Result<PathBuf> {
    anyhow::bail!("not implemented")
}
