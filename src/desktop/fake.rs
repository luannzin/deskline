//! Test backend: two fake monitors with a moving bar at 30 fps, and injected input recorded for
//! asserts.
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::{Frame, Input, Monitor};

pub static INJECTED: Mutex<Vec<Input>> = Mutex::new(Vec::new());
const SIZES: [(usize, usize); 2] = [(640, 360), (320, 240)];

pub struct Screen {
    task: tokio::task::JoinHandle<()>,
    current: Arc<AtomicUsize>,
}

impl Screen {
    pub async fn open(_home: &Path, _fresh: bool) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        let (tx, rx) = watch::channel(None);
        let current = Arc::new(AtomicUsize::new(0));
        let which = current.clone();
        let task = tokio::spawn(async move {
            for t in 0usize.. {
                let (w, h) = SIZES[which.load(Ordering::Relaxed)];
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
        Ok((Self { task, current }, rx))
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        SIZES
            .iter()
            .enumerate()
            .map(|(i, &(w, h))| Monitor { name: format!("Fake {}", i + 1), w: w as u32, h: h as u32, primary: i == 0 })
            .collect()
    }

    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    pub async fn select(&mut self, index: usize) -> anyhow::Result<()> {
        self.current.store(index, Ordering::Relaxed);
        Ok(())
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        INJECTED.lock().unwrap().push(i);
        Ok(())
    }

    pub async fn privacy(&mut self, _on: bool) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn input_allowed(&self) -> bool {
        true
    }

    pub fn set_fps(&mut self, _fps: u32) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn close(self) {
        self.task.abort();
    }
}
