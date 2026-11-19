//! Clipboard sync: notice when this computer's clipboard changes (text, or files copied in a file
//! manager), and set it from the other side without echoing that back.
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

/// Bigger copies stay local: they'd stall the control stream.
const MAX: usize = 1 << 20;

/// What the clipboard holds now.
pub enum Change {
    Text(String),
    /// Files or folders copied in a file manager; none when they've been replaced.
    Files(Vec<PathBuf>),
}

enum Put {
    Text(String),
    /// Answers whether the files are on the clipboard now.
    Files(Vec<PathBuf>, oneshot::Sender<bool>),
    /// Answers with the files on it now.
    Query(oneshot::Sender<Vec<PathBuf>>),
}

/// Watching this computer's clipboard. Dropping it stops.
pub struct Clipboard {
    tx: Sender<Put>,
    changes: mpsc::UnboundedReceiver<Change>,
}

impl Clipboard {
    /// Changes from now on arrive through `changed`; what's on the clipboard already doesn't.
    pub fn watch() -> anyhow::Result<Self> {
        let (tx, set) = channel::<Put>();
        let (out, changes) = mpsc::unbounded_channel();
        let (ready_tx, ready) = channel();
        // Polling: no OS has a portable change notification, and twice a second is plenty.
        std::thread::Builder::new().name("clipboard".into()).spawn(move || {
            let cb = arboard::Clipboard::new();
            let _ = ready_tx.send(cb.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("no clipboard: {e}")));
            let Ok(mut cb) = cb else { return };
            let files = |cb: &mut arboard::Clipboard| cb.get().file_list().unwrap_or_default();
            let (mut last, mut last_files) = (cb.get_text().ok(), files(&mut cb));
            loop {
                match set.recv_timeout(Duration::from_millis(500)) {
                    Ok(Put::Text(text)) => {
                        if cb.set_text(text.clone()).is_ok() {
                            last = Some(text);
                        }
                    }
                    Ok(Put::Files(paths, done)) => {
                        let ok = cb.set().file_list(&paths).is_ok();
                        if ok {
                            last_files = paths;
                        }
                        let _ = done.send(ok);
                    }
                    Ok(Put::Query(back)) => {
                        let _ = back.send(files(&mut cb));
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        // Files first: file managers offer their paths as text too.
                        let now = files(&mut cb);
                        if now != last_files {
                            last_files = now.clone();
                            if out.send(Change::Files(now)).is_err() {
                                return;
                            }
                        }
                        if !last_files.is_empty() {
                            continue;
                        }
                        let Ok(text) = cb.get_text() else { continue };
                        if last.as_ref() != Some(&text) {
                            last = Some(text.clone());
                            if text.len() <= MAX && out.send(Change::Text(text)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;
        ready.recv()??;
        Ok(Self { tx, changes })
    }

    pub fn set(&self, text: String) {
        if text.len() <= MAX {
            let _ = self.tx.send(Put::Text(text));
        }
    }

    /// Put files on the clipboard, as if copied in a file manager. Returns once they're there
    /// (false if the system refused), so a paste right after gets them.
    pub async fn set_files(&self, paths: Vec<PathBuf>) -> bool {
        let (done, ok) = oneshot::channel();
        let _ = self.tx.send(Put::Files(paths, done));
        ok.await.unwrap_or(false)
    }

    /// The files on the clipboard now (none if it holds something else).
    pub async fn files(&self) -> Vec<PathBuf> {
        let (back, files) = oneshot::channel();
        let _ = self.tx.send(Put::Query(back));
        files.await.unwrap_or_default()
    }
}

/// The next local change, or never while there's no clipboard being watched (no control).
pub async fn changed(c: &mut Option<Clipboard>) -> Option<Change> {
    match c {
        Some(c) => c.changes.recv().await,
        None => std::future::pending().await,
    }
}
