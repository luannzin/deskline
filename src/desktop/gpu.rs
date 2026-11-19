//! Windows: a captured picture that stays on the GPU. An encoder on the same GPU scales and
//! converts it there and never touches its pixels (codec/mf.rs); any other encoder reads it back
//! to memory once. At 4K that's the difference between a few milliseconds a frame and thirty.
use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct3D11::*;

/// A BGRA picture on the device that captured it.
#[derive(Clone)]
pub struct Gpu {
    pub tex: ID3D11Texture2D,
    /// The capture's CPU-readable texture of the same size, shared by its frames.
    staging: Arc<Mutex<ID3D11Texture2D>>,
}

impl Gpu {
    #[cfg_attr(test, allow(dead_code))]
    pub fn new(tex: ID3D11Texture2D, staging: Arc<Mutex<ID3D11Texture2D>>) -> Self {
        Self { tex, staging }
    }

    pub fn device(&self) -> windows::core::Result<ID3D11Device> {
        unsafe { self.tex.GetDevice() }
    }

    /// The picture in memory, rows packed. The device is multithread-protected (the capture
    /// sets that up), so any thread may call this.
    pub fn read(&self) -> anyhow::Result<Vec<u8>> {
        let staging = self.staging.lock().unwrap();
        unsafe {
            let ctx = self.device()?.GetImmediateContext()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            self.tex.GetDesc(&mut desc);
            let (w, h) = (desc.Width as usize, desc.Height as usize);
            ctx.CopyResource(&*staging, &self.tex);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&*staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let pitch = map.RowPitch as usize;
            let src = std::slice::from_raw_parts(map.pData as *const u8, pitch * h);
            let mut px = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                px.extend_from_slice(&src[y * pitch..][..w * 4]);
            }
            ctx.Unmap(&*staging, 0);
            Ok(px)
        }
    }
}
