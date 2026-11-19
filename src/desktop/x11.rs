//! X11 sessions without the RemoteDesktop portal (XFCE, Cinnamon, MATE, i3…): pictures of the
//! root window through shared memory, the pointer drawn in from XFixes, and input through XTEST.
//! Monitors are RandR CRTCs; changing them during a session isn't followed.
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use anyhow::Context;
use tokio::sync::watch;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::{randr, shm, xfixes};
use x11rb::rust_connection::RustConnection;

use super::Frames;
use crate::desktop::{Frame, Input, Monitor, Pool, gap};

/// X buttons for `Input::Button`: left, right, middle, back, forward.
const BUTTONS: [u8; 5] = [1, 3, 2, 8, 9];

/// A monitor in root window coordinates.
#[derive(Clone, Copy)]
struct Rect {
    x: i16,
    y: i16,
    w: u16,
    h: u16,
}

pub struct Screen {
    conn: RustConnection,
    root: Window,
    monitors: Vec<(Monitor, Rect)>,
    current: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
}

impl Screen {
    pub fn open() -> anyhow::Result<(Self, Frames)> {
        let (conn, n) = x11rb::connect(None).context("couldn't reach the X server")?;
        let setup = conn.setup();
        let screen = &setup.roots[n];
        let (root, depth) = (screen.root, screen.root_depth);
        let full = Rect { x: 0, y: 0, w: screen.width_in_pixels, h: screen.height_in_pixels };
        // 24-bit color in 32-bit little-endian pixels is BGRx, what the encoder takes.
        let bpp = setup.pixmap_formats.iter().find(|f| f.depth == depth).map(|f| f.bits_per_pixel);
        anyhow::ensure!(
            matches!(depth, 24 | 32) && bpp == Some(32) && setup.image_byte_order == ImageOrder::LSB_FIRST,
            "this X screen's {depth}-bit color can't be shared"
        );

        let mut monitors = crtcs(&conn, root).unwrap_or_default();
        if monitors.is_empty() {
            let m = Monitor { name: "Screen".into(), w: full.w.into(), h: full.h.into(), primary: true };
            monitors.push((m, full));
        }
        let first = monitors.iter().position(|(m, _)| m.primary).unwrap_or(0);
        let current = Arc::new(AtomicUsize::new(first));
        let stop = Arc::new(AtomicBool::new(false));
        let fps = Arc::new(AtomicU32::new(0));

        let (tx, frames) = watch::channel(None);
        let (ready_tx, ready) = mpsc::channel();
        let rects: Vec<Rect> = monitors.iter().map(|(_, r)| *r).collect();
        let (which, quit, rate) = (current.clone(), stop.clone(), fps.clone());
        std::thread::Builder::new().name("x11".into()).spawn(move || {
            if let Err(e) = capture(&rects, &which, &quit, &rate, &tx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        ready.recv().context("capture thread died")??;
        Ok((Self { conn, root, monitors, current, stop, fps }, frames))
    }

    pub fn set_fps(&self, fps: u32) {
        self.fps.store(fps, Ordering::Relaxed);
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.monitors.iter().map(|(m, _)| m.clone()).collect()
    }

    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    pub fn select(&mut self, index: usize) -> anyhow::Result<()> {
        self.current.store(index, Ordering::Relaxed);
        Ok(())
    }

    pub fn input(&self, i: Input) -> anyhow::Result<()> {
        use xproto::{BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT};
        let fake = |kind: u8, detail: u8, (x, y): (i16, i16)| -> anyhow::Result<()> {
            self.conn
                .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)
                .context("the X server refused remote input (XTEST)")?
                .ignore_error();
            Ok(())
        };
        match i {
            Input::Move { x, y } => fake(MOTION_NOTIFY_EVENT, 0, to_root(self.monitors[self.current()].1, x, y))?,
            Input::Button { button, down } => {
                if let Some(&b) = BUTTONS.get(button as usize) {
                    fake(if down { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT }, b, (0, 0))?
                }
            }
            Input::Scroll { x, y } => {
                for b in wheel(x, y) {
                    fake(BUTTON_PRESS_EVENT, b, (0, 0))?;
                    fake(BUTTON_RELEASE_EVENT, b, (0, 0))?;
                }
            }
            // X keycodes are evdev codes plus 8.
            Input::Key { code, down } => {
                if let Ok(k) = u8::try_from(code.saturating_add(8)) {
                    fake(if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT }, k, (0, 0))?
                }
            }
            // XTEST motion with detail 1 is relative.
            Input::MoveBy { dx, dy } => {
                let step = |d: i32| d.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
                fake(MOTION_NOTIFY_EVENT, 1, (step(dx), step(dy)))?
            }
            Input::CtrlAltDel => {}
        }
        self.conn.flush()?;
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The lit RandR CRTCs, named after their outputs.
fn crtcs(conn: &RustConnection, root: Window) -> anyhow::Result<Vec<(Monitor, Rect)>> {
    if conn.extension_information(randr::X11_EXTENSION_NAME)?.is_none() {
        return Ok(Vec::new());
    }
    conn.randr_query_version(1, 3)?.reply()?;
    let res = conn.randr_get_screen_resources_current(root)?.reply()?;
    let primary = conn.randr_get_output_primary(root)?.reply()?.output;
    let mut found = Vec::new();
    for &crtc in &res.crtcs {
        let c = conn.randr_get_crtc_info(crtc, res.config_timestamp)?.reply()?;
        let Some(&output) = c.outputs.first() else { continue };
        if c.width == 0 || c.height == 0 {
            continue;
        }
        let name = conn.randr_get_output_info(output, res.config_timestamp)?.reply()?.name;
        let m = Monitor {
            name: String::from_utf8_lossy(&name).into_owned(),
            w: c.width.into(),
            h: c.height.into(),
            primary: c.outputs.contains(&primary),
        };
        found.push((m, Rect { x: c.x, y: c.y, w: c.width, h: c.height }));
    }
    Ok(found)
}

/// Pictures the selected monitor at most `fps` times a second until `stop`, sending only changes.
fn capture(
    rects: &[Rect],
    current: &AtomicUsize,
    stop: &AtomicBool,
    fps: &AtomicU32,
    frames: &watch::Sender<Option<Arc<Frame>>>,
    ready: &mpsc::Sender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let (conn, n) = x11rb::connect(None)?;
    let root = conn.setup().roots[n].root;
    let biggest = rects.iter().map(|r| r.w as usize * r.h as usize * 4).max().unwrap_or(0);
    // Shared memory only works with a local X server; otherwise the pixels come in the reply.
    let shm = Shm::new(&conn, biggest).ok();
    let cursor = conn.extension_information(xfixes::X11_EXTENSION_NAME)?.is_some()
        && conn.xfixes_query_version(4, 0)?.reply().is_ok();
    let mut pool = Pool::default();
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let r = rects[current.load(Ordering::Relaxed)];
        let px = grab(&conn, root, r, shm.as_ref(), &mut pool)?;
        let mut frame = Frame { w: r.w.into(), h: r.h.into(), bgra: true, px };
        if cursor {
            let c = conn.xfixes_get_cursor_image()?.reply()?;
            let x = i32::from(c.x) - i32::from(c.xhot) - i32::from(r.x);
            let y = i32::from(c.y) - i32::from(c.yhot) - i32::from(r.y);
            draw_cursor(&mut frame, x, y, c.width.into(), &c.cursor_image);
        }
        // The borrow ends with the condition, before the send.
        if frames.borrow().as_ref().is_none_or(|l| l.w != frame.w || l.px != frame.px) {
            pool.send(frames, frame);
        } else {
            pool.recycle(frame);
        }
        // Keyboard mapping changes reach every client; drop them so they don't pile up.
        while let Ok(Some(_)) = conn.poll_for_event() {}
        std::thread::sleep(gap(fps.load(Ordering::Relaxed)).saturating_sub(started.elapsed()));
    }
    Ok(())
}

fn grab(conn: &RustConnection, root: Window, r: Rect, shm: Option<&Shm>, pool: &mut Pool) -> anyhow::Result<Vec<u8>> {
    let len = r.w as usize * r.h as usize * 4;
    if let Some(shm) = shm {
        conn.shm_get_image(root, r.x, r.y, r.w, r.h, !0, ImageFormat::Z_PIXMAP.into(), shm.seg, 0)?.reply()?;
        let mut px = pool.take(len);
        px.extend_from_slice(&shm.bytes()[..len]);
        return Ok(px);
    }
    let mut px = conn.get_image(ImageFormat::Z_PIXMAP, root, r.x, r.y, r.w, r.h, !0)?.reply()?.data;
    anyhow::ensure!(px.len() >= len, "the X server sent a short picture");
    px.truncate(len);
    Ok(px)
}

/// A System V shared memory segment the X server writes pictures into, skipping the socket.
struct Shm {
    seg: shm::Seg,
    ptr: *mut u8,
    len: usize,
}

impl Shm {
    fn new(conn: &RustConnection, len: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(conn.extension_information(shm::X11_EXTENSION_NAME)?.is_some(), "no MIT-SHM");
        let seg = conn.generate_id()?;
        // SAFETY: plain SysV calls; the pointer is checked before use and detached on drop.
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600);
            anyhow::ensure!(id >= 0, "shmget failed");
            let ptr = libc::shmat(id, std::ptr::null(), 0);
            if ptr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                anyhow::bail!("shmat failed");
            }
            let shm = Self { seg, ptr: ptr.cast(), len };
            let attached = conn.shm_attach(seg, id as u32, false).map(|c| c.check());
            // Marked for removal once the server holds it too: the kernel frees it when both
            // sides detach, even after a crash.
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            attached??;
            Ok(shm)
        }
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of `len` bytes until drop.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from shmat and is detached once.
        unsafe { libc::shmdt(self.ptr as *const libc::c_void) };
    }
}

/// A 0..1 position on the monitor to root window pixels.
fn to_root(r: Rect, x: f32, y: f32) -> (i16, i16) {
    let at = |start: i16, len: u16, f: f32| start + (f.clamp(0.0, 1.0) * f32::from(len.max(1) - 1)).round() as i16;
    (at(r.x, r.w, x), at(r.y, r.h, y))
}

/// X has no scroll events: wheel notches are clicks of buttons 4 (up), 5 (down), 6 (left) and
/// 7 (right). At most 100 notches a go so a bad value can't stall input.
fn wheel(x: i32, y: i32) -> impl Iterator<Item = u8> {
    let notches = |n: i32| n.unsigned_abs().min(100) as usize;
    let vertical = std::iter::repeat_n(if y > 0 { 4 } else { 5 }, notches(y));
    vertical.chain(std::iter::repeat_n(if x > 0 { 7 } else { 6 }, notches(x)))
}

/// Draws the cursor (premultiplied ARGB, `cw` wide) onto a BGRx frame with its top-left corner
/// at (x, y), clipped to the frame.
fn draw_cursor(f: &mut Frame, x: i32, y: i32, cw: usize, image: &[u32]) {
    if cw == 0 {
        return;
    }
    for (row, line) in image.chunks_exact(cw).enumerate() {
        let fy = y + row as i32;
        if fy < 0 || fy >= f.h as i32 {
            continue;
        }
        for (col, &p) in line.iter().enumerate() {
            let fx = x + col as i32;
            let a = p >> 24;
            if fx < 0 || fx >= f.w as i32 || a == 0 {
                continue;
            }
            let i = (fy as usize * f.w + fx as usize) * 4;
            for (c, shift) in [0, 8, 16].into_iter().enumerate() {
                let src = (p >> shift) & 0xff;
                let dst = u32::from(f.px[i + c]);
                f.px[i + c] = (src + dst * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_land_on_the_selected_monitor() {
        let right = Rect { x: 1920, y: 0, w: 1280, h: 1024 };
        assert_eq!(to_root(right, 0.0, 0.0), (1920, 0));
        assert_eq!(to_root(right, 1.0, 1.0), (1920 + 1279, 1023));
        assert_eq!(to_root(right, 0.5, 0.5), (1920 + 640, 512));
        assert_eq!(to_root(right, -3.0, 7.0), (1920, 1023));
    }

    #[test]
    fn wheel_notches_are_buttons() {
        assert_eq!(wheel(0, 2).collect::<Vec<_>>(), [4, 4]);
        assert_eq!(wheel(-1, -1).collect::<Vec<_>>(), [5, 6]);
        assert_eq!(wheel(3, 0).collect::<Vec<_>>(), [7, 7, 7]);
        assert_eq!(wheel(0, i32::MIN).count(), 100);
        assert_eq!(BUTTONS, [1, 3, 2, 8, 9]);
    }

    #[test]
    fn cursor_blends_and_clips() {
        let mut f = Frame { w: 3, h: 2, bgra: true, px: vec![100; 3 * 2 * 4] };
        // 2x2 cursor at (2, -1): only its bottom-left pixel (opaque red) lands, on (2, 0).
        let red = 0xff_ff_00_00;
        let half_white = 0x80_80_80_80;
        draw_cursor(&mut f, 2, -1, 2, &[half_white, half_white, red, 0]);
        assert_eq!(&f.px[8..12], &[0, 0, 255, 100]);
        // Half-transparent white over 100: 128 + 100 * 127 / 255 = 177.
        let mut g = Frame { w: 1, h: 1, bgra: true, px: vec![100; 4] };
        draw_cursor(&mut g, 0, 0, 1, &[half_white]);
        assert_eq!(&g.px[..3], &[177, 177, 177]);
        // Everything else untouched.
        assert!(f.px[..8].iter().chain(&f.px[12..]).all(|&b| b == 100));
    }
}
