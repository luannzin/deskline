//! Test backend: a moving bar at 30 fps, and injected input recorded for asserts.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::{Frame, Input};

pub static INJECTED: Mutex<Vec<Input>> = Mutex::new(Vec::new());

pub struct Screen {
    task: tokio::task::JoinHandle<()>,
}

impl Screen {
    pub async fn open(_home: &Path) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        let (tx, rx) = watch::channel(None);
        let task = tokio::spawn(async move {
            for t in 0usize.. {
                let (w, h) = (640, 360);
                let mut px = vec![40u8; w * h * 4];
                for y in 0..h {
                    for x in (t * 8 % w)..((t * 8 % w) + 16).min(w) {
                        px[(y * w + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
                    }
                }
                tx.send_replace(Some(Arc::new(Frame { w, h, bgra: true, px })));
                tokio::time::sleep(Duration::from_millis(33)).await;
            }
        });
        Ok((Self { task }, rx))
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        INJECTED.lock().unwrap().push(i);
        Ok(())
    }

    pub async fn close(self) {
        self.task.abort();
    }
}
