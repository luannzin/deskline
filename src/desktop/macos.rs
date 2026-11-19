//! macOS: a CoreGraphics display stream gives the frames (pointer included, only when something
//! changed) and CGEvent posts the viewer's input. Needs the Screen Recording permission to share
//! and Accessibility to be controlled; macOS asks for each the first time.
// CGDisplayStream is deprecated in favor of ScreenCaptureKit but still works, and is far simpler.
#![allow(deprecated)]
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayPixelsHigh,
    CGDisplayPixelsWide, CGDisplayStream, CGDisplayStreamFrameStatus, CGDisplayStreamUpdate, CGError, CGEvent,
    CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGGetActiveDisplayList, CGMainDisplayID,
    CGMouseButton, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGScrollEventUnit,
    kCGDisplayStreamMinimumFrameTime, kCGDisplayStreamShowCursor,
};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use tokio::sync::watch;

use super::{DEFAULT_FPS, Frame, Input, Monitor};

// ponytail: fixed at the macOS default; the user's own setting needs AppKit.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// Modifier keys: evdev code and the event flags they set (the shared mask plus the left/right
/// bit some apps check).
const MODIFIERS: [(u16, u64); 8] = [
    (42, CGEventFlags::MaskShift.0 | 0x02),      // left shift
    (54, CGEventFlags::MaskShift.0 | 0x04),      // right shift
    (29, CGEventFlags::MaskControl.0 | 0x01),    // left control
    (97, CGEventFlags::MaskControl.0 | 0x2000),  // right control
    (56, CGEventFlags::MaskAlternate.0 | 0x20),  // left alt (option)
    (100, CGEventFlags::MaskAlternate.0 | 0x40), // right alt (option)
    (125, CGEventFlags::MaskCommand.0 | 0x08),   // left meta (command)
    (126, CGEventFlags::MaskCommand.0 | 0x10),   // right meta (command)
];
/// Every flag bit `MODIFIERS` and caps lock control.
const ALL_MODS: u64 = CGEventFlags::MaskShift.0
    | CGEventFlags::MaskControl.0
    | CGEventFlags::MaskAlternate.0
    | CGEventFlags::MaskCommand.0
    | CGEventFlags::MaskAlphaShift.0
    | 0x207F;

pub struct Screen {
    displays: Vec<Display>,
    current: usize,
    frames: watch::Sender<Option<Arc<Frame>>>,
    /// The display whose frames get through: a stream being replaced may still deliver a few.
    want: Arc<AtomicU32>,
    /// Dropping it stops the capture thread, which owns every CoreGraphics object, so `Screen`
    /// itself is plain data and channels (and `Send`).
    _capture: mpsc::Sender<()>,
    /// Most frames a second the stream delivers.
    fps: u32,
    /// Held modifier keys, one bit per `MODIFIERS` entry.
    mods: u8,
    caps: bool,
    /// Held mouse buttons, one bit per button.
    buttons: u8,
    /// Last button press: when, which, and how many in a row (for double and triple clicks).
    click: (Instant, u8, i64),
    trusted: bool,
    asked: bool,
}

impl Screen {
    pub async fn open(_home: &Path, _fresh: bool) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        if !CGPreflightScreenCaptureAccess() {
            // Shows the system prompt (once) and lists Deskline in the settings pane.
            let _ = CGRequestScreenCaptureAccess();
            anyhow::bail!(
                "Allow Deskline under System Settings → Privacy & Security → Screen Recording, then try again."
            );
        }
        let displays = displays()?;
        let (frames, rx) = watch::channel(None);
        let want = Arc::new(AtomicU32::new(displays[0].id));
        let capture = start(&displays[0], frames.clone(), want.clone(), DEFAULT_FPS)?;
        let screen = Self {
            displays,
            current: 0,
            frames,
            want,
            _capture: capture,
            fps: DEFAULT_FPS,
            mods: 0,
            caps: false,
            buttons: 0,
            click: (Instant::now(), u8::MAX, 0),
            trusted: false,
            asked: false,
        };
        Ok((screen, rx))
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.displays
            .iter()
            .enumerate()
            .map(|(i, d)| Monitor {
                name: format!("Display {}{}", i + 1, if d.main { " (main)" } else { "" }),
                w: d.px.0 as u32,
                h: d.px.1 as u32,
                primary: d.main,
            })
            .collect()
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub async fn select(&mut self, index: usize) -> anyhow::Result<()> {
        let d = *self.displays.get(index).ok_or_else(|| anyhow::anyhow!("There's no display {}.", index + 1))?;
        if index != self.current {
            // Let the new display through first, so its first frame isn't filtered out.
            let old = self.want.swap(d.id, Ordering::SeqCst);
            let capture = start(&d, self.frames.clone(), self.want.clone(), self.fps)
                .inspect_err(|_| self.want.store(old, Ordering::SeqCst))?;
            self._capture = capture; // drops the old sender, which stops the old stream
            self.current = index;
        }
        Ok(())
    }

    pub fn input_allowed(&self) -> bool {
        true
    }

    /// The stream's rate is fixed when it starts: a new rate is a new stream.
    pub fn set_fps(&mut self, fps: u32) -> anyhow::Result<()> {
        if fps != self.fps {
            self._capture = start(&self.displays[self.current], self.frames.clone(), self.want.clone(), fps)?;
            self.fps = fps;
        }
        Ok(())
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        if matches!(i, Input::CtrlAltDel) {
            return Ok(());
        }
        if !self.trusted {
            // Only the first check shows the system prompt; later ones notice when it's granted.
            self.trusted = accessibility(!self.asked);
            self.asked = true;
            anyhow::ensure!(
                self.trusted,
                "Allow Deskline under System Settings → Privacy & Security → Accessibility, then try again."
            );
        }
        match i {
            Input::Move { x, y } => {
                // Points, not pixels: the display's place in the global desktop. max/min drop NaN.
                let r = self.displays[self.current].bounds;
                let at = CGPoint::new(
                    r.origin.x + x.max(0.0).min(1.0) as f64 * (r.size.width - 1.0),
                    r.origin.y + y.max(0.0).min(1.0) as f64 * (r.size.height - 1.0),
                );
                self.moved(at, None);
            }
            // Games read the deltas; the position keeps the pointer in step for everyone else.
            Input::MoveBy { dx, dy } => {
                let p = cursor();
                self.moved(CGPoint::new(p.x + f64::from(dx), p.y + f64::from(dy)), Some((dx, dy)));
            }
            // 0 left, 1 right, 2 middle, 3 back, 4 forward: Quartz numbers them the same way.
            Input::Button { button, down } => {
                if button > 4 {
                    return Ok(());
                }
                let kind = match (button, down) {
                    (0, true) => CGEventType::LeftMouseDown,
                    (0, false) => CGEventType::LeftMouseUp,
                    (1, true) => CGEventType::RightMouseDown,
                    (1, false) => CGEventType::RightMouseUp,
                    (_, true) => CGEventType::OtherMouseDown,
                    (_, false) => CGEventType::OtherMouseUp,
                };
                if down {
                    self.buttons |= 1 << button;
                    let (last, which, count) = self.click;
                    let count = if which == button && last.elapsed() < DOUBLE_CLICK { count + 1 } else { 1 };
                    self.click = (Instant::now(), button, count);
                } else {
                    self.buttons &= !(1 << button);
                }
                // Without the click count, a quick second click is just another single click.
                let clicks = if self.click.1 == button { self.click.2 } else { 1 };
                let e = CGEvent::new_mouse_event(None, kind, cursor(), CGMouseButton(button as u32));
                if let Some(e) = &e {
                    CGEvent::set_integer_value_field(Some(&**e), CGEventField::MouseEventClickState, clicks);
                    CGEvent::set_integer_value_field(Some(&**e), CGEventField::MouseEventButtonNumber, button as i64);
                }
                self.post(e);
            }
            // Wheel 1 is vertical, positive up; wheel 2 is horizontal, positive left.
            Input::Scroll { x, y } => {
                self.post(CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 2, y, x.saturating_neg(), 0));
            }
            Input::Key { code, down } => {
                if let Some(i) = MODIFIERS.iter().position(|m| m.0 == code) {
                    if down {
                        self.mods |= 1 << i;
                    } else {
                        self.mods &= !(1 << i);
                    }
                }
                if code == 58 && down {
                    self.caps = !self.caps;
                }
                let Some(key) = keycode(code) else { return Ok(()) };
                self.post(CGEvent::new_keyboard_event(None, key, down));
            }
            Input::CtrlAltDel => {}
        }
        Ok(())
    }

    pub async fn privacy(&mut self, on: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!on, "Privacy mode isn't available on macOS.");
        Ok(())
    }

    pub async fn close(self) {
        // Dropping `_capture` stops the stream.
    }

    /// The pointer to `at`, with `delta` as the movement apps see.
    fn moved(&self, at: CGPoint, delta: Option<(i32, i32)>) {
        // With a button held, apps only follow drag events (selections, window moves).
        let (kind, button) = match self.buttons {
            0 => (CGEventType::MouseMoved, 0),
            b if b & 1 != 0 => (CGEventType::LeftMouseDragged, 0),
            b if b & 2 != 0 => (CGEventType::RightMouseDragged, 1),
            b => (CGEventType::OtherMouseDragged, b.trailing_zeros()),
        };
        let e = CGEvent::new_mouse_event(None, kind, at, CGMouseButton(button));
        if let (Some(e), Some((dx, dy))) = (&e, delta) {
            CGEvent::set_integer_value_field(Some(&**e), CGEventField::MouseEventDeltaX, dx.into());
            CGEvent::set_integer_value_field(Some(&**e), CGEventField::MouseEventDeltaY, dy.into());
        }
        self.post(e);
    }

    /// Posts as if from the keyboard and mouse, with the viewer's modifiers (and only theirs) held,
    /// so shift-click and command-click work and this computer's own keys don't leak in.
    fn post(&self, e: Option<CFRetained<CGEvent>>) {
        let Some(e) = e else { return };
        let mut mods =
            MODIFIERS.iter().enumerate().filter(|&(i, _)| self.mods & (1 << i) != 0).fold(0, |f, (_, m)| f | m.1);
        if self.caps {
            mods |= CGEventFlags::MaskAlphaShift.0;
        }
        // Keep what the system set itself (keypad, fn), replace the modifiers.
        let others = CGEvent::flags(Some(&*e)).0 & !ALL_MODS;
        CGEvent::set_flags(Some(&*e), CGEventFlags(others | mods));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&*e));
    }
}

/// One display: where it sits in the global desktop, in points (for the mouse), and its size in
/// pixels (for capture; twice the points on Retina).
#[derive(Clone, Copy)]
struct Display {
    id: CGDirectDisplayID,
    bounds: CGRect,
    px: (usize, usize),
    main: bool,
}

/// The active displays, main first.
fn displays() -> anyhow::Result<Vec<Display>> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    // SAFETY: both pointers are valid, and the list has room for `ids.len()` displays.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) };
    anyhow::ensure!(err == CGError::Success && n > 0, "No display found.");
    let main = CGMainDisplayID();
    let mut list: Vec<Display> = ids
        .iter()
        .take(n as usize)
        .map(|&id| {
            let px = match CGDisplayCopyDisplayMode(id) {
                Some(m) => (CGDisplayMode::pixel_width(Some(&*m)), CGDisplayMode::pixel_height(Some(&*m))),
                None => (CGDisplayPixelsWide(id), CGDisplayPixelsHigh(id)),
            };
            Display { id, bounds: CGDisplayBounds(id), px, main: id == main }
        })
        .collect();
    list.sort_by_key(|d| !d.main);
    Ok(list)
}

/// Starts streaming `d` at most `fps` times a second on its own thread and waits until it runs.
/// Dropping the returned sender stops it.
fn start(
    d: &Display,
    frames: watch::Sender<Option<Arc<Frame>>>,
    want: Arc<AtomicU32>,
    fps: u32,
) -> anyhow::Result<mpsc::Sender<()>> {
    let (stop, stop_rx) = mpsc::channel();
    let (ready_tx, ready) = mpsc::channel();
    let (id, px) = (d.id, d.px);
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || capture(id, px, fps, frames, want, stop_rx, ready_tx))?;
    tokio::task::block_in_place(|| ready.recv())??;
    Ok(stop)
}

/// Streams one display until `stop`'s sender is dropped. The stream calls back on its own serial
/// dispatch queue; everything the handler touches is thread-safe (watch sender, atomic, channel).
fn capture(
    display: CGDirectDisplayID,
    (w, h): (usize, usize),
    fps: u32,
    frames: watch::Sender<Option<Arc<Frame>>>,
    want: Arc<AtomicU32>,
    stop: mpsc::Receiver<()>,
    ready: mpsc::Sender<anyhow::Result<()>>,
) {
    let (stopped_tx, stopped) = mpsc::channel();
    let handler = RcBlock::new(
        move |status: CGDisplayStreamFrameStatus,
              _time: u64,
              surface: *mut IOSurfaceRef,
              _update: *const CGDisplayStreamUpdate| {
            if status == CGDisplayStreamFrameStatus::Stopped {
                let _ = stopped_tx.send(());
            } else if status == CGDisplayStreamFrameStatus::FrameComplete && want.load(Ordering::SeqCst) == display {
                // SAFETY: the surface (or null) is valid for the length of this call.
                if let Some(f) = unsafe { surface.as_ref() }.and_then(copy) {
                    frames.send_replace(Some(Arc::new(f)));
                }
            }
        },
    );
    // Pointer drawn in, and at most `fps`: 30 is plenty for a desktop. Idle screens send nothing.
    let keys: [&CFString; 2] = unsafe { [kCGDisplayStreamShowCursor, kCGDisplayStreamMinimumFrameTime] };
    let fps = CFNumber::new_f64(1.0 / f64::from(fps.max(1)));
    let values: [&CFType; 2] = [CFBoolean::new(true), &fps];
    let props = CFDictionary::from_slices(&keys, &values);
    let queue = DispatchQueue::new("deskline.capture", None);
    let bgra = i32::from_be_bytes(*b"BGRA");
    // SAFETY: the properties use the documented key and value types, and the handler has the
    // stream's callback signature. The stream copies the block, and we keep ours alive anyway.
    let stream = unsafe {
        CGDisplayStream::with_dispatch_queue(
            display,
            w,
            h,
            bgra,
            Some(props.as_opaque()),
            &queue,
            RcBlock::as_ptr(&handler),
        )
    };
    let Some(stream) = stream.filter(|s| CGDisplayStream::start(Some(&**s)) == CGError::Success) else {
        let _ = ready.send(Err(anyhow::anyhow!("Couldn't start capturing the screen.")));
        return;
    };
    let _ = ready.send(Ok(()));
    // ponytail: an unplugged display ends its stream and frames stop; picking a display restarts.
    let _ = stop.recv();
    CGDisplayStream::stop(Some(&*stream));
    // Release the stream only once its handler is done with the last frame.
    let _ = stopped.recv_timeout(Duration::from_secs(1));
}

/// Copies a BGRA surface into a packed frame, dropping each row's padding.
fn copy(s: &IOSurfaceRef) -> Option<Frame> {
    // SAFETY: a read-only lock, released below; no seed wanted.
    if unsafe { s.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) } != 0 {
        return None;
    }
    let (w, h, stride) = (s.width(), s.height(), s.bytes_per_row());
    let base = s.base_address().as_ptr().cast::<u8>().cast_const();
    let mut px = Vec::with_capacity(w * h * 4);
    if stride >= w * 4 {
        for y in 0..h {
            // SAFETY: the locked surface holds `h` rows of `stride` bytes each.
            px.extend_from_slice(unsafe { std::slice::from_raw_parts(base.add(y * stride), w * 4) });
        }
    }
    // SAFETY: matches the lock above.
    unsafe { s.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    (w > 0 && px.len() == w * h * 4).then(|| Frame { w, h, bgra: true, px })
}

/// Where the pointer is now: clicks land there.
fn cursor() -> CGPoint {
    CGEvent::new(None).map_or(CGPoint::default(), |e| CGEvent::location(Some(&*e)))
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: &'static CFString;
    fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> u8;
}

/// Whether macOS lets Deskline post input. `prompt` shows the system's Accessibility dialog.
fn accessibility(prompt: bool) -> bool {
    // SAFETY: a system string constant, and a dictionary of the documented key and value types.
    unsafe {
        let opts = CFDictionary::from_slices(&[kAXTrustedCheckOptionPrompt], &[CFBoolean::new(prompt)]);
        AXIsProcessTrustedWithOptions(Some(opts.as_opaque())) != 0
    }
}

/// evdev code to Mac virtual key code (kVK_*, by position like evdev).
const fn keycode(code: u16) -> Option<u16> {
    Some(match code {
        1 => 0x35,   // escape
        2 => 0x12,   // 1
        3 => 0x13,   // 2
        4 => 0x14,   // 3
        5 => 0x15,   // 4
        6 => 0x17,   // 5
        7 => 0x16,   // 6
        8 => 0x1A,   // 7
        9 => 0x1C,   // 8
        10 => 0x19,  // 9
        11 => 0x1D,  // 0
        12 => 0x1B,  // -
        13 => 0x18,  // =
        14 => 0x33,  // backspace (the Mac "delete" key)
        15 => 0x30,  // tab
        16 => 0x0C,  // q
        17 => 0x0D,  // w
        18 => 0x0E,  // e
        19 => 0x0F,  // r
        20 => 0x11,  // t
        21 => 0x10,  // y
        22 => 0x20,  // u
        23 => 0x22,  // i
        24 => 0x1F,  // o
        25 => 0x23,  // p
        26 => 0x21,  // [
        27 => 0x1E,  // ]
        28 => 0x24,  // return
        29 => 0x3B,  // left control
        30 => 0x00,  // a
        31 => 0x01,  // s
        32 => 0x02,  // d
        33 => 0x03,  // f
        34 => 0x05,  // g
        35 => 0x04,  // h
        36 => 0x26,  // j
        37 => 0x28,  // k
        38 => 0x25,  // l
        39 => 0x29,  // ;
        40 => 0x27,  // '
        41 => 0x32,  // `
        42 => 0x38,  // left shift
        43 => 0x2A,  // backslash
        44 => 0x06,  // z
        45 => 0x07,  // x
        46 => 0x08,  // c
        47 => 0x09,  // v
        48 => 0x0B,  // b
        49 => 0x2D,  // n
        50 => 0x2E,  // m
        51 => 0x2B,  // ,
        52 => 0x2F,  // .
        53 => 0x2C,  // /
        54 => 0x3C,  // right shift
        55 => 0x43,  // keypad *
        56 => 0x3A,  // left alt (option)
        57 => 0x31,  // space
        58 => 0x39,  // caps lock
        59 => 0x7A,  // F1
        60 => 0x78,  // F2
        61 => 0x63,  // F3
        62 => 0x76,  // F4
        63 => 0x60,  // F5
        64 => 0x61,  // F6
        65 => 0x62,  // F7
        66 => 0x64,  // F8
        67 => 0x65,  // F9
        68 => 0x6D,  // F10
        69 => 0x47,  // num lock: keypad clear sits there
        71 => 0x59,  // keypad 7
        72 => 0x5B,  // keypad 8
        73 => 0x5C,  // keypad 9
        74 => 0x4E,  // keypad -
        75 => 0x56,  // keypad 4
        76 => 0x57,  // keypad 5
        77 => 0x58,  // keypad 6
        78 => 0x45,  // keypad +
        79 => 0x53,  // keypad 1
        80 => 0x54,  // keypad 2
        81 => 0x55,  // keypad 3
        82 => 0x52,  // keypad 0
        83 => 0x41,  // keypad .
        86 => 0x0A,  // the extra key left of Z on ISO keyboards
        87 => 0x67,  // F11
        88 => 0x6F,  // F12
        96 => 0x4C,  // keypad enter
        97 => 0x3E,  // right control
        98 => 0x4B,  // keypad /
        100 => 0x3D, // right alt (option)
        102 => 0x73, // home
        103 => 0x7E, // up
        104 => 0x74, // page up
        105 => 0x7B, // left
        106 => 0x7C, // right
        107 => 0x77, // end
        108 => 0x7D, // down
        109 => 0x79, // page down
        110 => 0x72, // insert: Mac keyboards have Help there
        111 => 0x75, // delete (forward delete)
        117 => 0x51, // keypad =
        125 => 0x37, // left meta (command)
        126 => 0x36, // right meta (command)
        183 => 0x69, // F13
        184 => 0x6B, // F14
        185 => 0x71, // F15
        186 => 0x6A, // F16
        187 => 0x40, // F17
        188 => 0x4F, // F18
        189 => 0x50, // F19
        190 => 0x5A, // F20
        _ => return None,
    })
}

// No two evdev codes may land on the same Mac key; checked every time this file compiles (tests
// use the fake backend, so a #[test] here would never run).
const _: () = {
    let mut seen = [false; 128];
    let mut code = 0;
    while code < 256 {
        if let Some(k) = keycode(code) {
            assert!(!seen[k as usize], "two evdev codes map to the same Mac key");
            seen[k as usize] = true;
        }
        code += 1;
    }
};
