//! Windows: DXGI desktop duplication of any monitor (GPU copy, only on change) with the mouse
//! pointer drawn in, and SendInput. Run as SYSTEM (the installed service), it follows the sign-in
//! screen, lock screen and UAC prompts onto the secure desktop. Run as a normal user it can't see
//! those, nor click into elevated (admin) windows unless Deskline runs as admin.
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, w};

use super::{Frame, Gpu, Input, Monitor, Pool, gap};
use crate::keyhook;

/// Only channels, atomics and plain data: it's shared across tokio tasks. Windows, desktops and
/// DXGI objects stay on the threads that made them.
pub struct Screen {
    monitors: Vec<Monitor>,
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    blank: Option<Blank>,
}

/// What the capture and input threads share with `Screen`.
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    /// Index into `monitors` of the one to share.
    want: AtomicUsize,
    /// Where the shared monitor sits on the desktop, to aim the mouse.
    rect: Mutex<RECT>,
    /// Most frames a second to capture (0: the default).
    fps: AtomicU32,
}

enum Job {
    Input(Input),
    /// BlockInput on or off; answers whether it worked.
    Block(bool, oneshot::Sender<bool>),
}

impl Screen {
    pub async fn open(_home: &Path, _fresh: bool) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        let shared = Arc::new(Shared::default());
        let (jobs, rx) = mpsc::channel();
        let s = shared.clone();
        std::thread::Builder::new().name("input".into()).spawn(move || inject(rx, s))?;
        let (tx, frames) = watch::channel(None);
        let (ready_tx, ready) = oneshot::channel();
        let s = shared.clone();
        std::thread::Builder::new().name("capture".into()).spawn(move || capture(tx, s, ready_tx))?;
        let monitors = ready.await??;
        Ok((Self { monitors, shared, jobs, blank: None }, frames))
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        let _ = self.jobs.send(Job::Input(i));
        Ok(())
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.monitors.clone()
    }

    pub fn current(&self) -> usize {
        self.shared.want.load(Ordering::SeqCst)
    }

    pub async fn select(&mut self, index: usize) -> anyhow::Result<()> {
        self.shared.want.store(index, Ordering::SeqCst);
        Ok(())
    }

    pub fn input_allowed(&self) -> bool {
        true
    }

    pub fn set_fps(&mut self, fps: u32) -> anyhow::Result<()> {
        self.shared.fps.store(fps, Ordering::SeqCst);
        Ok(())
    }

    pub async fn privacy(&mut self, on: bool) -> anyhow::Result<()> {
        if !on {
            self.blank = None;
        } else if self.blank.is_none() {
            self.blank = Some(Blank::open().await?);
        }
        // Only the thread that called BlockInput may still inject, so the input thread calls it.
        // Ctrl+Alt+Del on this computer lifts it: that's Windows' way out for the person here.
        let (tx, rx) = oneshot::channel();
        let _ = self.jobs.send(Job::Block(on, tx));
        if on && !rx.await.unwrap_or(false) {
            eprintln!(
                "privacy mode: the screen is blank, but this computer's mouse and keyboard still work \
                 (ignoring them needs Deskline to run as administrator)"
            );
        }
        Ok(())
    }

    /// Dropping stops both threads and undoes privacy mode.
    pub async fn close(self) {}
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

/// Real pixels for this thread's coordinates, like DXGI's, even if the process isn't DPI aware.
fn dpi_aware() {
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// The bounding box of all monitors: x, y, width, height.
fn virtual_screen() -> (i32, i32, i32, i32) {
    let m = |i| unsafe { GetSystemMetrics(i) };
    (m(SM_XVIRTUALSCREEN), m(SM_YVIRTUALSCREEN), m(SM_CXVIRTUALSCREEN), m(SM_CYVIRTUALSCREEN))
}

/// The desktop a thread captures or injects on: Default normally, Winlogon for the sign-in
/// screen, lock screen and UAC prompts. Only SYSTEM may open Winlogon; others stay on Default.
struct Desktop {
    original: HDESK,
    current: Option<HDESK>,
    name: String,
}

impl Desktop {
    fn new() -> Self {
        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap_or(HDESK(std::ptr::null_mut()));
        Self { original, current: None, name: String::new() }
    }

    /// Move this thread to the desktop that gets input now, if that changed. Works only while
    /// the thread owns no windows or hooks.
    fn follow(&mut self) {
        unsafe {
            let Ok(input) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0)) else {
                return;
            };
            let mut buf = [0u16; 64];
            let _ = GetUserObjectInformationW(HANDLE(input.0), UOI_NAME, Some(buf.as_mut_ptr().cast()), size_of_val(&buf) as u32, None);
            let name = String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
            if name == self.name || SetThreadDesktop(input).is_err() {
                let _ = CloseDesktop(input);
                return;
            }
            if let Some(old) = self.current.replace(input) {
                let _ = CloseDesktop(old);
            }
            self.name = name;
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // A desktop can't be closed while a thread is on it.
        if let Some(d) = self.current.take() {
            unsafe {
                if SetThreadDesktop(self.original).is_ok() {
                    let _ = CloseDesktop(d);
                }
            }
        }
    }
}

/// The input thread: injects on whichever desktop gets input now.
fn inject(jobs: mpsc::Receiver<Job>, shared: Arc<Shared>) {
    dpi_aware();
    let mut desktop = Desktop::new();
    let mut blocked = false;
    for job in jobs {
        desktop.follow();
        match job {
            Job::Input(i) => send_input(i, *shared.rect.lock().unwrap()),
            Job::Block(on, done) => {
                let ok = unsafe { BlockInput(on) }.is_ok();
                blocked = on && ok;
                let _ = done.send(ok);
            }
        }
    }
    if blocked {
        let _ = unsafe { BlockInput(false) };
    }
}

/// `shared` is the shared monitor's desktop rectangle.
fn send_input(i: Input, shared: RECT) {
    match i {
        Input::Move { x, y } => {
            // Absolute positions span all monitors: aim at the pixel's centre in the shared one.
            let (vx, vy, vw, vh) = virtual_screen();
            let at = |v: f32, lo: i32, hi: i32, vlo: i32, vsize: i32| {
                let size = (hi - lo) as f64;
                let px = lo as f64 + (v.clamp(0.0, 1.0) as f64 * size).floor().min(size - 1.0);
                ((px - vlo as f64 + 0.5) * 65536.0 / vsize as f64) as i32
            };
            mouse(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                at(x, shared.left, shared.right, vx, vw),
                at(y, shared.top, shared.bottom, vy, vh),
                0,
            )
        }
        Input::Button { button, down } => {
            // (press, lift, mouseData) for left, right, middle, back, forward.
            let buttons = [
                (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
                (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
                (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
                (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
                (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
            ];
            let Some(&(press, lift, data)) = buttons.get(button as usize) else { return };
            mouse(if down { press } else { lift }, 0, 0, data as i32)
        }
        Input::Scroll { x, y } => {
            // At most 100 notches a go, like X11: a bad value can't overflow 120 per notch.
            let (x, y) = (x.clamp(-100, 100), y.clamp(-100, 100));
            if y != 0 {
                mouse(MOUSEEVENTF_WHEEL, 0, 0, y * 120)
            }
            if x != 0 {
                mouse(MOUSEEVENTF_HWHEEL, 0, 0, x * 120)
            }
        }
        Input::CtrlAltDel => crate::service::request_sas(),
        // Without ABSOLUTE the counts go to the pointer as is, and to raw-input games.
        Input::MoveBy { dx, dy } => mouse(MOUSEEVENTF_MOVE, dx, dy, 0),
        Input::Key { code, down } => {
            let Some((scan, extended, vk)) = keyhook::to_windows(code) else { return };
            let mut flags = if vk == 0 { KEYEVENTF_SCANCODE } else { KEYBD_EVENT_FLAGS(0) };
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            let ki = KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
            send(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } });
        }
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) {
    let mi = MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    send(INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi } });
}

fn send(input: INPUT) {
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

/// Every monitor on the desktop with the adapter that drives it.
fn outputs() -> windows::core::Result<Vec<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC)>> {
    let mut all = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                let desc = output.GetDesc()?;
                if desc.AttachedToDesktop.as_bool() {
                    all.push((adapter.clone(), output, desc));
                }
                o += 1;
            }
            a += 1;
        }
    }
    Ok(all)
}

/// The monitors to offer, with their DXGI device names (which survive mode changes): the primary
/// (at desktop origin 0,0) first, then left to right.
// ponytail: listed once per session; a monitor plugged in later shows up on the next share.
fn monitors() -> anyhow::Result<Vec<(Monitor, [u16; 32])>> {
    let mut descs: Vec<_> = outputs()?.into_iter().map(|(_, _, d)| d).collect();
    anyhow::ensure!(!descs.is_empty(), "No monitor found.");
    descs.sort_by_key(|d| {
        let r = d.DesktopCoordinates;
        (r.left != 0 || r.top != 0, r.left, r.top)
    });
    let list = descs.iter().enumerate().map(|(i, d)| {
        let r = d.DesktopCoordinates;
        let primary = r.left == 0 && r.top == 0;
        let name = format!("Display {}{}", i + 1, if primary { " (primary)" } else { "" });
        (Monitor { name, w: (r.right - r.left) as u32, h: (r.bottom - r.top) as u32, primary }, d.DeviceName)
    });
    Ok(list.collect())
}

/// The capture thread: duplicates the wanted monitor, starting over when the duplication is lost
/// or another monitor is picked.
fn capture(tx: watch::Sender<Option<Arc<Frame>>>, shared: Arc<Shared>, ready: oneshot::Sender<anyhow::Result<Vec<Monitor>>>) {
    dpi_aware();
    let mut desktop = Desktop::new();
    desktop.follow();
    let (first, outs) = match monitors().and_then(|m| Ok((Dup::open(&m[0].1)?, m))) {
        Ok(o) => o,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    *shared.rect.lock().unwrap() = first.rect;
    let _ = ready.send(Ok(outs.iter().map(|o| o.0.clone()).collect()));
    let (mut dup, mut current) = (Some(first), 0);
    while !shared.stop.load(Ordering::SeqCst) {
        let want = shared.want.load(Ordering::SeqCst);
        if want != current {
            (dup, current) = (None, want);
        }
        let Some(d) = dup.as_mut() else {
            // Resolution change, fullscreen switch, sign-in screen, UAC prompt or another monitor:
            // move to the desktop in front and start over (the old duplication is gone by now).
            desktop.follow();
            match Dup::open(&outs[current].1) {
                Ok(d) => {
                    *shared.rect.lock().unwrap() = d.rect;
                    dup = Some(d);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
            continue;
        };
        let started = Instant::now();
        match d.next() {
            Ok(Some(f)) => {
                d.pool.send(&tx, f);
                // 30 fps is plenty for a desktop and halves the work of 60 Hz screens; a game
                // asks for more.
                std::thread::sleep(gap(shared.fps.load(Ordering::SeqCst)).saturating_sub(started.elapsed()));
            }
            Ok(None) => {}
            Err(_) => dup = None,
        }
    }
}

/// The pointer's patch of the picture is at most this square; a bigger pointer is cut off.
const PATCH: u32 = 256;

/// One monitor's duplication. Pictures stay on the GPU: each frame is a texture of its own (the
/// desktop, then the pointer drawn over its patch), which an encoder on this GPU converts and
/// encodes there (codec/mf.rs), and others read back (`Gpu::read`).
struct Dup {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// The last desktop picture, without the pointer.
    desk: ID3D11Texture2D,
    /// CPU-readable, whole pictures, for `Gpu::read`.
    staging: Arc<Mutex<ID3D11Texture2D>>,
    /// CPU-readable `PATCH` square: the desktop under the pointer.
    patch: ID3D11Texture2D,
    w: usize,
    h: usize,
    rect: RECT,
    fresh: bool,
    pointer: Pointer,
    /// Frame textures, reused once no session holds them.
    pool: Pool,
}

impl Dup {
    fn open(device: &[u16; 32]) -> anyhow::Result<Dup> {
        let (adapter, output, desc) = outputs()?
            .into_iter()
            .find(|o| o.2.DeviceName == *device)
            .ok_or_else(|| anyhow::anyhow!("That screen is no longer connected."))?;
        unsafe {
            // Video support lets the encoder use this device's video processor; a driver
            // without it still captures.
            let video = D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0);
            let create = |flags| {
                let (mut device, mut ctx) = (None, None);
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut ctx),
                )
                .map(|()| (device.expect("device"), ctx.expect("context")))
            };
            let (device, ctx) = create(video).or_else(|_| create(D3D11_CREATE_DEVICE_BGRA_SUPPORT))?;
            // Frames are read and encoded on other threads than this one.
            let _ = ctx.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
            let dup = output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)?;
            let mode = dup.GetDesc().ModeDesc;
            let (w, h) = (mode.Width, mode.Height);
            let readable = |w, h| texture(&device, w, h, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32);
            Ok(Dup {
                desk: texture(&device, w, h, D3D11_USAGE_DEFAULT, FRAME_BIND, 0)?,
                staging: Arc::new(Mutex::new(readable(w, h)?)),
                patch: readable(PATCH, PATCH)?,
                device,
                ctx,
                dup,
                w: w as usize,
                h: h as usize,
                rect: desc.DesktopCoordinates,
                fresh: true,
                pointer: Pointer::default(),
                pool: Pool::default(),
            })
        }
    }

    /// Ok(None) when neither the desktop nor the pointer changed within the timeout.
    fn next(&mut self) -> windows::core::Result<Option<Frame>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res = None;
            if let Err(e) = self.dup.AcquireNextFrame(100, &mut info, &mut res) {
                return if e.code() == DXGI_ERROR_WAIT_TIMEOUT { Ok(None) } else { Err(e) };
            }
            // LastPresentTime 0: only the mouse moved or changed shape.
            let desktop = self.fresh || info.LastPresentTime != 0;
            let pointer = info.LastMouseUpdateTime != 0 && self.pointer.update(&self.dup, &info)?;
            if desktop {
                let tex: ID3D11Texture2D = res.expect("frame").cast()?;
                self.ctx.CopyResource(&self.desk, &tex);
                self.fresh = false;
            }
            self.dup.ReleaseFrame()?;
            if !desktop && !pointer {
                return Ok(None);
            }
            let out = match self.pool.reuse().and_then(|f| f.gpu) {
                Some(g) => g.tex,
                None => texture(&self.device, self.w as u32, self.h as u32, D3D11_USAGE_DEFAULT, FRAME_BIND, 0)?,
            };
            self.ctx.CopyResource(&out, &self.desk);
            self.overlay(&out)?;
            let gpu = Some(Gpu { tex: out, staging: self.staging.clone() });
            Ok(Some(Frame { w: self.w, h: self.h, bgra: true, px: Vec::new(), gpu }))
        }
    }

    /// The pointer over `out`: the patch of desktop under it comes back to memory, gets the
    /// pointer drawn the way Windows draws it, and goes up again. A few KB a frame, not the
    /// whole picture.
    unsafe fn overlay(&self, out: &ID3D11Texture2D) -> windows::core::Result<()> {
        let Some((x, y, w, h)) = self.pointer.area(self.w, self.h) else { return Ok(()) };
        let area = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
        let (w, h) = (w as usize, h as usize);
        unsafe {
            self.ctx.CopySubresourceRegion(&self.patch, 0, 0, 0, 0, &self.desk, 0, Some(&area));
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.patch, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let pitch = map.RowPitch as usize;
            let src = std::slice::from_raw_parts(map.pData as *const u8, pitch * h);
            let mut px = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                px.extend_from_slice(&src[row * pitch..][..w * 4]);
            }
            self.ctx.Unmap(&self.patch, 0);
            self.pointer.draw(&mut px, w, h, POINT { x: x as i32, y: y as i32 });
            self.ctx.UpdateSubresource(out, 0, Some(&area), px.as_ptr().cast(), (w * 4) as u32, 0);
        }
        Ok(())
    }
}

/// Frame textures: copied into, read by the video processor, and sampled.
const FRAME_BIND: u32 = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;

/// A BGRA texture on `device`.
unsafe fn texture(device: &ID3D11Device, w: u32, h: u32, usage: D3D11_USAGE, bind: u32, cpu: u32) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    Ok(tex.expect("texture"))
}

/// The mouse pointer: DXGI leaves it out of the picture and reports it separately.
#[derive(Default)]
struct Pointer {
    visible: bool,
    /// Top left of the shape, in monitor pixels.
    at: POINT,
    shape: Vec<u8>,
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
}

impl Pointer {
    /// Take a frame's pointer news. True when the picture needs drawing again.
    unsafe fn update(&mut self, dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) -> windows::core::Result<bool> {
        let p = info.PointerPosition;
        let visible = p.Visible.as_bool();
        let moved = visible != self.visible || (visible && p.Position != self.at);
        (self.visible, self.at) = (visible, p.Position);
        if info.PointerShapeBufferSize == 0 {
            return Ok(moved);
        }
        self.shape.resize(info.PointerShapeBufferSize as usize, 0);
        let mut len = 0;
        unsafe { dup.GetFramePointerShape(self.shape.len() as u32, self.shape.as_mut_ptr().cast(), &mut len, &mut self.info)? };
        Ok(true)
    }

    /// Monochrome shapes are an AND mask above an XOR mask, so half as tall as they say.
    fn rows(&self) -> u32 {
        let i = &self.info;
        if i.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32 { i.Height / 2 } else { i.Height }
    }

    /// Where it's drawn on a `w`x`h` monitor, at most `PATCH` square: x, y, width, height.
    fn area(&self, w: usize, h: usize) -> Option<(u32, u32, u32, u32)> {
        if !self.visible || self.shape.is_empty() {
            return None;
        }
        let (x0, y0) = (self.at.x.max(0), self.at.y.max(0));
        let x1 = (self.at.x + self.info.Width as i32).min(w as i32).min(x0 + PATCH as i32);
        let y1 = (self.at.y + self.rows() as i32).min(h as i32).min(y0 + PATCH as i32);
        (x1 > x0 && y1 > y0).then(|| (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Draw the pointer the way Windows does on screen, over a `w`x`h` BGRA patch of the
    /// monitor whose top left is `origin`.
    fn draw(&self, px: &mut [u8], w: usize, h: usize, origin: POINT) {
        let i = &self.info;
        if !self.visible || self.shape.is_empty() {
            return;
        }
        let mono = i.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
        let color = i.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32;
        let rows = self.rows() as usize;
        let pitch = i.Pitch as usize;
        for sy in 0..rows {
            let y = self.at.y - origin.y + sy as i32;
            if y < 0 || y >= h as i32 {
                continue;
            }
            for sx in 0..i.Width as usize {
                let x = self.at.x - origin.x + sx as i32;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let d = &mut px[(y as usize * w + x as usize) * 4..][..3];
                if mono {
                    let bit = |row: usize| self.shape.get(row * pitch + sx / 8).map_or(0, |b| (b >> (7 - sx % 8) & 1) * 0xFF);
                    let (and, xor) = (bit(sy), bit(sy + rows));
                    d.iter_mut().for_each(|c| *c = (*c & and) ^ xor);
                    continue;
                }
                let Some(s) = self.shape.get(sy * pitch + sx * 4..sy * pitch + sx * 4 + 4) else { continue };
                for c in 0..3 {
                    d[c] = if color {
                        let a = s[3] as u32;
                        ((s[c] as u32 * a + d[c] as u32 * (255 - a)) / 255) as u8
                    } else if s[3] == 0 {
                        // Masked color: alpha 0 replaces the screen pixel, 0xFF inverts it by XOR.
                        s[c]
                    } else {
                        d[c] ^ s[c]
                    };
                }
            }
        }
    }
}

/// Privacy mode's black window over every monitor, left out of capture so the viewer still sees
/// the desktop. It lives on its own thread: windows belong to the thread that made them.
struct Blank {
    /// The HWND as a number: Screen must stay Send.
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Blank {
    async fn open() -> anyhow::Result<Self> {
        let (tx, rx) = oneshot::channel();
        let thread = std::thread::Builder::new().name("privacy".into()).spawn(move || unsafe {
            dpi_aware();
            match blank_window() {
                Ok(hwnd) => {
                    let _ = tx.send(Ok(hwnd.0 as isize));
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
        })?;
        Ok(Self { hwnd: rx.await??, thread: Some(thread) })
    }
}

impl Drop for Blank {
    fn drop(&mut self) {
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

unsafe fn blank_window() -> anyhow::Result<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("DesklinePrivacy");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(blank_proc),
            hInstance: instance,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: class,
            ..Default::default()
        };
        // Fails harmlessly when an earlier privacy mode registered it.
        RegisterClassW(&wc);
        let (x, y, w, h) = virtual_screen();
        // Layered and transparent: the viewer's clicks go through to the windows underneath.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            class,
            w!("Deskline privacy"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance),
            None,
        )?;
        if SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).is_err()
            || SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err()
        {
            let _ = DestroyWindow(hwnd);
            anyhow::bail!("Privacy mode needs Windows 10 version 2004 or newer.");
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // Stay above windows raised later, and cover monitors added or moved.
        SetTimer(Some(hwnd), 1, 500, None);
        Ok(hwnd)
    }
}

unsafe extern "system" fn blank_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                SetTextColor(dc, COLORREF(0xFFFFFF));
                SetBkMode(dc, TRANSPARENT);
                // Centred on the primary monitor, which sits at desktop 0,0.
                let (x, y, _, _) = virtual_screen();
                let mut r = RECT {
                    left: -x,
                    top: -y,
                    right: GetSystemMetrics(SM_CXSCREEN) - x,
                    bottom: GetSystemMetrics(SM_CYSCREEN) - y,
                };
                let mut text: Vec<u16> = "This screen is in use remotely".encode_utf16().collect();
                DrawTextW(dc, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_TIMER => {
                let (x, y, w, h) = virtual_screen();
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
