//! Windows viewer: a low-level keyboard hook. While grabbing and Deskline's window has focus,
//! every key (Win, Alt+Tab, Print Screen, numpad, media keys…) goes to the remote screen instead
//! of this computer. Also the Linux evdev <-> Windows key table the Windows host injects with.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::Context;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static SINK: Mutex<Option<(Sender<(u16, bool)>, Context)>> = Mutex::new(None);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static GRAB: AtomicBool = AtomicBool::new(false);
/// Keys whose press went to the remote screen. Their release goes there too, even after the grab
/// ended, so neither side is left with a key stuck down.
static HELD: Mutex<[bool; 256]> = Mutex::new([false; 256]);

/// Install the hook on its own thread. Keys arrive as (Linux evdev code, down).
pub fn start(ctx: Context) -> Receiver<(u16, bool)> {
    let (tx, rx) = channel();
    *SINK.lock().unwrap() = Some((tx, ctx));
    if !INSTALLED.swap(true, Ordering::SeqCst) {
        let spawned = std::thread::Builder::new().name("keyhook".into()).spawn(|| unsafe {
            let module = GetModuleHandleW(None).ok().map(Into::into);
            if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0) {
                INSTALLED.store(false, Ordering::SeqCst);
                eprintln!("keyboard hook: {e}. Windows shortcuts will act on this computer.");
                return;
            }
            // Windows calls the hook from this thread's message loop.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
        });
        if spawned.is_err() {
            INSTALLED.store(false, Ordering::SeqCst);
        }
    }
    rx
}

/// Grab keys (while Deskline's window has focus) or let them through.
pub fn grab(on: bool) {
    GRAB.store(on, Ordering::SeqCst);
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Injected keys are our own (or another remote tool's): never grab those.
        if !k.flags.contains(LLKHF_INJECTED) && forward(k) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Send the key to the remote screen if it's meant for it. True when sent, so it's swallowed.
fn forward(k: &KBDLLHOOKSTRUCT) -> bool {
    let Some(code) = from_windows(k.scanCode as u16, k.flags.contains(LLKHF_EXTENDED), k.vkCode as u16) else {
        return false;
    };
    let down = !k.flags.contains(LLKHF_UP);
    let mut held = HELD.lock().unwrap();
    let held = &mut held[code as usize];
    let remote = if down { *held || (GRAB.load(Ordering::SeqCst) && in_front()) } else { *held };
    if !remote {
        return false;
    }
    *held = down;
    if let Some((tx, ctx)) = &*SINK.lock().unwrap() {
        let _ = tx.send((code, down));
        ctx.request_repaint();
    }
    true
}

/// The window with keyboard focus is one of ours.
fn in_front() -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

/// Keys sent and recognized by virtual key: Pause and Print Screen have no plain scancode, Num
/// Lock shares Pause's, and media keys only work this way. (evdev, scancode, extended, key)
const BY_VK: [(u16, u16, bool, VIRTUAL_KEY); 12] = [
    (69, 0x45, true, VK_NUMLOCK),
    (99, 0x37, true, VK_SNAPSHOT),
    (113, 0x20, true, VK_VOLUME_MUTE),
    (114, 0x2E, true, VK_VOLUME_DOWN),
    (115, 0x30, true, VK_VOLUME_UP),
    (119, 0x45, false, VK_PAUSE),
    (158, 0x6A, true, VK_BROWSER_BACK),
    (159, 0x69, true, VK_BROWSER_FORWARD),
    (163, 0x19, true, VK_MEDIA_NEXT_TRACK),
    (164, 0x22, true, VK_MEDIA_PLAY_PAUSE),
    (165, 0x10, true, VK_MEDIA_PREV_TRACK),
    (166, 0x24, true, VK_MEDIA_STOP),
];

/// Linux evdev code to Windows: (PC set-1 scancode, E0 extended, virtual key). Virtual key 0
/// means send the scancode, which the remote's keyboard layout then reads like a real key.
pub(crate) fn to_windows(code: u16) -> Option<(u16, bool, u16)> {
    if let Some(&(_, scan, ext, vk)) = BY_VK.iter().find(|k| k.0 == code) {
        return Some((scan, ext, vk.0));
    }
    let (scan, ext) = match code {
        // Main block, F1-F12 and the keypad are numbered the same in both.
        1..=83 | 86..=88 => (code, false),
        89 => (0x73, false),  // ABNT2 /? key
        96 => (0x1C, true),   // keypad enter
        97 => (0x1D, true),   // right ctrl
        98 => (0x35, true),   // keypad /
        100 => (0x38, true),  // right alt (AltGr)
        102 => (0x47, true),  // home
        103 => (0x48, true),  // up
        104 => (0x49, true),  // page up
        105 => (0x4B, true),  // left
        106 => (0x4D, true),  // right
        107 => (0x4F, true),  // end
        108 => (0x50, true),  // down
        109 => (0x51, true),  // page down
        110 => (0x52, true),  // insert
        111 => (0x53, true),  // delete
        121 => (0x7E, false), // ABNT2 keypad .
        125 => (0x5B, true),  // left windows
        126 => (0x5C, true),  // right windows
        127 => (0x5D, true),  // menu
        183..=193 => (0x64 + code - 183, false), // F13-F23
        194 => (0x76, false), // F24
        _ => return None,
    };
    Some((scan, ext, 0))
}

/// What the keyboard hook saw (scancode, extended flag, virtual key) to Linux evdev code.
pub(crate) fn from_windows(scan: u16, ext: bool, vk: u16) -> Option<u16> {
    if let Some(k) = BY_VK.iter().find(|k| k.3.0 == vk) {
        return Some(k.0);
    }
    (1..256).find(|&c| to_windows(c) == Some((scan, ext, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let mut supported = 0;
        for code in 0..256 {
            let Some((scan, ext, vk)) = to_windows(code) else { continue };
            assert_eq!(from_windows(scan, ext, vk), Some(code), "evdev {code}");
            supported += 1;
        }
        // Main block, keypad, locks, Print Screen, Pause, right Ctrl/Alt, Win, Menu, F1-F24,
        // browser and media keys.
        for code in [1, 14, 15, 28, 29, 42, 54, 56, 57, 58, 69, 70, 71, 82, 83, 55, 74, 78, 96, 98, 87, 88]
            .into_iter()
            .chain([97, 99, 100, 119, 125, 126, 127, 158, 159, 113, 114, 115, 163, 164, 165, 166])
            .chain(183..=194)
        {
            assert!(to_windows(code).is_some(), "evdev {code}");
        }
        assert!(supported > 110);
    }

    /// Everything the egui viewer can send has a Windows key.
    #[test]
    fn egui_keys_map() {
        for &k in eframe::egui::Key::ALL {
            if let Some(code) = crate::desktop::evdev(k) {
                assert!(to_windows(code).is_some(), "{k:?} = evdev {code}");
            }
        }
    }
}
