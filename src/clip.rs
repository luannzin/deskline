//! Clipboard sync: notice when this computer's clipboard text changes, and set it from the other
//! side without echoing that back.
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

use tokio::sync::mpsc;

/// Bigger copies stay local: they'd stall the control stream.
const MAX: usize = 1 << 20;

/// Watching this computer's clipboard. Dropping it stops.
pub struct Clipboard {
    tx: Sender<String>,
}

impl Clipboard {
    /// Changes arrive on the receiver; whatever is on the clipboard now doesn't.
    pub fn watch() -> anyhow::Result<(Self, mpsc::UnboundedReceiver<String>)> {
        let (tx, set) = channel::<String>();
        let (out, changes) = mpsc::unbounded_channel();
        let (ready_tx, ready) = channel();
        // Polling: no OS has a portable change notification, and twice a second is plenty.
        std::thread::Builder::new().name("clipboard".into()).spawn(move || {
            let mut cb = match arboard::Clipboard::new() {
                Ok(cb) => {
                    let _ = ready_tx.send(Ok(()));
                    cb
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow::anyhow!("no clipboard: {e}")));
                    return;
                }
            };
            let mut last = cb.get_text().ok();
            loop {
                match set.recv_timeout(Duration::from_millis(500)) {
                    Ok(text) => {
                        if cb.set_text(text.clone()).is_ok() {
                            last = Some(text);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        let Ok(text) = cb.get_text() else { continue };
                        if last.as_ref() != Some(&text) {
                            last = Some(text.clone());
                            if text.len() <= MAX && out.send(text).is_err() {
                                return;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;
        ready.recv()??;
        Ok((Self { tx }, changes))
    }

    pub fn set(&self, text: String) {
        if text.len() <= MAX {
            let _ = self.tx.send(text);
        }
    }
}
