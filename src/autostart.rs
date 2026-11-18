//! Start Deskline in the background (to the tray) when the user signs in: the Run key on Windows,
//! an XDG autostart entry on Linux, a LaunchAgent on macOS. Each points at this exe, so a copy
//! that was moved shows as off until it's turned on again.
// until ui.rs uses it
#![allow(dead_code)]

use anyhow::Context;
use std::path::Path;

/// Windows: the Run value, `"<exe>" --background` (a Windows path can't contain `"`).
#[cfg(windows)]
fn command(exe: &Path) -> String {
    format!("\"{}\" --background", exe.display())
}

#[cfg(windows)]
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// This exe starts at sign-in.
#[cfg(windows)]
pub fn enabled() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let value = windows_registry::CURRENT_USER.open(RUN).and_then(|k| k.get_string("Deskline"));
    value.is_ok_and(|v| v == command(&exe))
}

/// On: start `<this exe> --background` at sign-in. Off: remove that.
#[cfg(windows)]
pub fn set(on: bool) -> anyhow::Result<()> {
    let set = || -> anyhow::Result<()> {
        let key = windows_registry::CURRENT_USER.create(RUN)?;
        if on {
            key.set_string("Deskline", command(&std::env::current_exe()?))?;
        } else if key.get_type("Deskline").is_ok() {
            key.remove_value("Deskline")?;
        }
        Ok(())
    };
    set().context("Couldn't change whether Deskline starts at sign-in")
}

/// The entry file; `mention` is the line in it that names this exe.
#[cfg(unix)]
fn entry() -> anyhow::Result<std::path::PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    let home = || var("HOME").context("no home folder");
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/LaunchAgents/io.github.luannzin.deskline.plist"));
    }
    let config = match var("XDG_CONFIG_HOME") {
        Some(dir) => dir,
        None => home()?.join(".config"),
    };
    Ok(config.join("autostart/deskline.desktop"))
}

/// Linux: the `Exec=` line, the exe quoted per the Desktop Entry spec (it may hold spaces).
#[cfg(target_os = "linux")]
fn mention(exe: &Path) -> String {
    let mut q = String::new();
    for c in exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' => q.extend(['\\', c]),
            // escaped for the quotes, then again for the file's string syntax
            '\\' => q.push_str(r"\\\\"),
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    format!("Exec=\"{q}\" --background")
}

#[cfg(target_os = "linux")]
fn contents(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Deskline\nComment=Accept remote desktop sessions\n{}\nIcon=deskline\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        mention(exe)
    )
}

/// macOS: the program line of the LaunchAgent.
#[cfg(target_os = "macos")]
fn mention(exe: &Path) -> String {
    let path = exe.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<string>{path}</string>")
}

#[cfg(target_os = "macos")]
fn contents(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>io.github.luannzin.deskline</string>
    <key>ProgramArguments</key>
    <array>
        {}
        <string>--background</string>
    </array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
        mention(exe)
    )
}

/// This exe starts at sign-in.
#[cfg(unix)]
pub fn enabled() -> bool {
    let (Ok(file), Ok(exe)) = (entry(), std::env::current_exe()) else { return false };
    std::fs::read_to_string(file).is_ok_and(|s| s.lines().any(|l| l.trim() == mention(&exe)))
}

/// On: start `<this exe> --background` at sign-in. Off: remove that.
#[cfg(unix)]
pub fn set(on: bool) -> anyhow::Result<()> {
    let set = || -> anyhow::Result<()> {
        let file = entry()?;
        if on {
            std::fs::create_dir_all(file.parent().context("no folder")?)?;
            // written for the next sign-in only: loading it now would start a second Deskline
            std::fs::write(&file, contents(&std::env::current_exe()?))?;
        } else if let Err(e) = std::fs::remove_file(&file)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e.into());
        }
        Ok(())
    };
    set().context("Couldn't change whether Deskline starts at sign-in")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn run_value_quotes_the_exe() {
        assert_eq!(command(Path::new(r"C:\Program Files\Deskline\deskline.exe")), r#""C:\Program Files\Deskline\deskline.exe" --background"#);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exec_line_escapes_the_exe() {
        assert_eq!(mention(Path::new("/usr/bin/deskline")), r#"Exec="/usr/bin/deskline" --background"#);
        assert_eq!(mention(Path::new(r#"/home/a b/$x"100%\d"#)), r#"Exec="/home/a b/\$x\"100%%\\\\d" --background"#);
        assert!(contents(Path::new("/usr/bin/deskline")).lines().any(|l| l == mention(Path::new("/usr/bin/deskline"))));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_agent_escapes_the_exe() {
        let exe = Path::new("/Applications/A&B <1>.app/Contents/MacOS/deskline");
        assert_eq!(mention(exe), "<string>/Applications/A&amp;B &lt;1&gt;.app/Contents/MacOS/deskline</string>");
        assert!(contents(exe).lines().any(|l| l.trim() == mention(exe)));
    }
}
