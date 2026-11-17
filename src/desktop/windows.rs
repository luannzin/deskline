//! Windows: DXGI desktop duplication of the primary monitor (GPU copy, only on change) and
//! SendInput. Can't see or click into elevated (admin) windows unless Deskline runs as admin.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::core::Interface;

use super::{Frame, Input};

pub struct Screen {
    stop: Arc<AtomicBool>,
}

impl Screen {
    pub async fn open(_home: &Path) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        let (tx, frames) = watch::channel(None);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let flag = stop.clone();
        std::thread::Builder::new().name("capture".into()).spawn(move || capture(tx, flag, ready_tx))?;
        tokio::task::block_in_place(|| ready.recv())??;
        Ok((Self { stop }, frames))
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        match i {
            // Absolute without VIRTUALDESK spans the primary monitor, the one being shared.
            Input::Move { x, y } => mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                (x.clamp(0.0, 1.0) * 65535.0) as i32,
                (y.clamp(0.0, 1.0) * 65535.0) as i32,
                0,
            ),
            Input::Button { button, down } => {
                let flags = match (button, down) {
                    (0, true) => MOUSEEVENTF_LEFTDOWN,
                    (0, false) => MOUSEEVENTF_LEFTUP,
                    (1, true) => MOUSEEVENTF_RIGHTDOWN,
                    (1, false) => MOUSEEVENTF_RIGHTUP,
                    (_, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (_, false) => MOUSEEVENTF_MIDDLEUP,
                };
                mouse(flags, 0, 0, 0)
            }
            Input::Scroll { steps } => mouse(MOUSEEVENTF_WHEEL, 0, 0, steps * 120),
            Input::Key { code, down } => {
                let Some((scan, extended)) = scancode(code) else { return Ok(()) };
                let mut flags = KEYEVENTF_SCANCODE;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP;
                }
                let ki = KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
                send(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } });
            }
        }
        Ok(())
    }

    pub async fn close(self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) {
    let mi = MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    send(INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi } });
}

fn send(input: INPUT) {
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

/// evdev code to PC set-1 scancode. The main block is numbered the same in both.
fn scancode(code: u16) -> Option<(u16, bool)> {
    Some(match code {
        1..=88 => (code, false),
        96 => (0x1C, true),  // keypad enter
        97 => (0x1D, true),  // right ctrl
        98 => (0x35, true),  // keypad /
        100 => (0x38, true), // right alt
        102 => (0x47, true), // home
        103 => (0x48, true), // up
        104 => (0x49, true), // page up
        105 => (0x4B, true), // left
        106 => (0x4D, true), // right
        107 => (0x4F, true), // end
        108 => (0x50, true), // down
        109 => (0x51, true), // page down
        110 => (0x52, true), // insert
        111 => (0x53, true), // delete
        125 => (0x5B, true), // left windows
        _ => return None,
    })
}

struct Dup {
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    w: usize,
    h: usize,
}

/// The output at desktop origin (0,0) is the primary monitor.
fn open_dup() -> anyhow::Result<Dup> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut found = None;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                let r = output.GetDesc()?.DesktopCoordinates;
                if r.left == 0 && r.top == 0 {
                    found = Some((adapter.clone(), output));
                    break;
                }
                o += 1;
            }
            if found.is_some() {
                break;
            }
            a += 1;
        }
        let (adapter, output) = found.ok_or_else(|| anyhow::anyhow!("no monitor found"))?;

        let (mut device, mut ctx) = (None, None);
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )?;
        let (device, ctx) = (device.expect("device"), ctx.expect("context"));
        let dup = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
        let mode = dup.GetDesc().ModeDesc;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: mode.Width,
            Height: mode.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        Ok(Dup { ctx, dup, staging: staging.expect("texture"), w: mode.Width as usize, h: mode.Height as usize })
    }
}

impl Dup {
    /// Ok(None) when nothing changed within the timeout.
    fn next(&mut self, first: bool) -> windows::core::Result<Option<Frame>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res = None;
            if let Err(e) = self.dup.AcquireNextFrame(100, &mut info, &mut res) {
                return if e.code() == DXGI_ERROR_WAIT_TIMEOUT { Ok(None) } else { Err(e) };
            }
            // LastPresentTime 0: only the mouse moved.
            let changed = first || info.LastPresentTime != 0;
            if changed {
                let tex: ID3D11Texture2D = res.expect("frame").cast()?;
                self.ctx.CopyResource(&self.staging, &tex);
            }
            self.dup.ReleaseFrame()?;
            if !changed {
                return Ok(None);
            }
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let pitch = map.RowPitch as usize;
            let src = std::slice::from_raw_parts(map.pData as *const u8, pitch * self.h);
            let mut px = Vec::with_capacity(self.w * self.h * 4);
            for y in 0..self.h {
                px.extend_from_slice(&src[y * pitch..][..self.w * 4]);
            }
            self.ctx.Unmap(&self.staging, 0);
            Ok(Some(Frame { w: self.w, h: self.h, bgra: true, px }))
        }
    }
}

fn capture(tx: watch::Sender<Option<Arc<Frame>>>, stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<anyhow::Result<()>>) {
    let mut dup = match open_dup() {
        Ok(d) => {
            let _ = ready.send(Ok(()));
            d
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match dup.next(first) {
            Ok(Some(f)) => {
                first = false;
                tx.send_replace(Some(Arc::new(f)));
                // 30 fps is plenty for a desktop and halves the work of 60 Hz screens.
                std::thread::sleep(Duration::from_millis(33).saturating_sub(started.elapsed()));
            }
            Ok(None) => {}
            // Resolution change, UAC prompt, fullscreen switch: start over.
            Err(_) => loop {
                std::thread::sleep(Duration::from_millis(250));
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(d) = open_dup() {
                    dup = d;
                    first = true;
                    break;
                }
            },
        }
    }
}
