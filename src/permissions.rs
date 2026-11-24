//! macOS: what this Mac lets Deskline do. Sharing the screen needs Screen Recording, being
//! controlled needs Accessibility. macOS asks once for each and keeps the answer in System
//! Settings, against the app's signature.
use std::path::Path;
use std::process::Command;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

/// Info.plist's CFBundleIdentifier (tools/build-macos.sh): what System Settings files answers under.
const BUNDLE: &str = "io.github.luannzin.deskline";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Access {
    Screen,
    Control,
}

impl Access {
    pub const ALL: [Access; 2] = [Access::Screen, Access::Control];

    pub fn granted(self) -> bool {
        match self {
            Access::Screen => CGPreflightScreenCaptureAccess(),
            Access::Control => accessibility(false),
        }
    }

    /// The first time, the system's own question (it also lists Deskline in the pane). macOS asks
    /// that only once, so after it the pane opens, where the switch is.
    pub fn ask(self, first: bool) {
        match (self, first) {
            (Access::Screen, true) => {
                let _ = CGRequestScreenCaptureAccess();
            }
            (Access::Control, true) => {
                let _ = accessibility(true);
            }
            _ => {
                let pane = format!("x-apple.systempreferences:com.apple.preference.security?{}", self.names().0);
                let _ = Command::new("open").arg(pane).spawn();
            }
        }
    }

    /// Its pane in System Settings, and its service's name for tccutil.
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Access::Screen => ("Privacy_ScreenCapture", "ScreenCapture"),
            Access::Control => ("Privacy_Accessibility", "Accessibility"),
        }
    }
}

/// After an update, an answer macOS keeps for the old signature shows as switched on in System
/// Settings yet lets nothing through, and macOS won't ask again over it. Up to 0.8.0 every build
/// was ad-hoc, known only by its hash, so this happened on every update. Clearing the stale
/// answer lets the home page ask afresh. Only what isn't allowed is cleared: a working answer
/// stays.
pub fn after_update(home: &Path) {
    let file = home.join("version");
    let now = env!("CARGO_PKG_VERSION");
    if std::fs::read_to_string(&file).is_ok_and(|v| v.trim() == now) {
        return;
    }
    for a in Access::ALL {
        if !a.granted() {
            let _ = Command::new("tccutil").args(["reset", a.names().1, BUNDLE]).status();
        }
    }
    let _ = std::fs::write(file, now);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: &'static CFString;
    fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> u8;
}

/// Whether macOS lets Deskline post input. `prompt` shows the system's Accessibility dialog.
pub fn accessibility(prompt: bool) -> bool {
    // SAFETY: a system string constant, and a dictionary of the documented key and value types.
    unsafe {
        let opts = CFDictionary::from_slices(&[kAXTrustedCheckOptionPrompt], &[CFBoolean::new(prompt)]);
        AXIsProcessTrustedWithOptions(Some(opts.as_opaque())) != 0
    }
}
