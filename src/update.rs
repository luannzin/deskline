//! Updates from GitHub Releases: a background thread looks for a newer release now and then and
//! downloads it; the user installs it from the window and Deskline restarts as the new version.
//! The installed Windows copy is updated silently by its service instead (`run_in_service`).
//! Trust is HTTPS to this repo's releases, the same as downloading the file by hand.

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

const REPO: &str = "luannzin/deskline";
/// This platform's file on every release (built by .github/workflows/release.yml).
#[cfg(windows)]
const ASSET: &str = "deskline.exe";
#[cfg(target_os = "linux")]
const ASSET: &str = "deskline_amd64.deb";
#[cfg(target_os = "macos")]
const ASSET: &str = "Deskline.dmg";
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// Windows: where an update moves the exe it replaced (it may still be running).
#[cfg(windows)]
const OLD: &str = "deskline.old.exe";

pub enum Msg {
    /// A newer version is downloaded: (release tag, file).
    Ready(String, PathBuf),
    /// Installed; call `relaunch()` and quit.
    Installed,
    /// Show to the user as is: why the update failed, or (a copy that can't update itself)
    /// where to get the new version.
    Failed(String),
}

pub struct Updater {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub ready: Option<(String, PathBuf)>,
    pub installing: bool,
}

impl Updater {
    /// Checks now and every few hours in the background; downloads into `dir`. Only Deskline's
    /// own downloads in `dir` are ever deleted.
    pub fn start(dir: PathBuf) -> Updater {
        let (tx, rx) = channel();
        // dev builds (cargo run) aren't installed, so there's nothing to update
        if !cfg!(debug_assertions) && !service_updates_us() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                loop {
                    match check(&dir) {
                        Ok(Some(msg)) => {
                            let _ = tx.send(msg);
                            return;
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("deskline: update check failed: {e:#}"),
                    }
                    std::thread::sleep(EVERY);
                }
            });
        }
        Updater { tx, rx, ready: None, installing: false }
    }

    /// Install `ready` off the UI thread (Linux asks for a password); `Installed` or `Failed` follows.
    pub fn install(&mut self) {
        let Some((_, file)) = self.ready.clone() else { return };
        // The window disables its button meanwhile.
        self.installing = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(match install(&file) {
                Ok(()) => Msg::Installed,
                Err(e) => Msg::Failed(format!("{e:#}")),
            });
        });
    }
}

/// The latest release's file, downloaded and checked into `dir`, when it's newer than us; else
/// clears out old downloads.
fn check(dir: &Path) -> anyhow::Result<Option<Msg>> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let tag = agent().get(url).call()?.body_mut().read_json::<Release>()?.tag_name;
    if !newer(&tag, env!("CARGO_PKG_VERSION")) {
        for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if f.file_name().to_string_lossy().contains(&format!("-{ASSET}")) {
                let _ = std::fs::remove_file(f.path());
            }
        }
        return Ok(None);
    }
    if !self_updates() {
        return Ok(Some(Msg::Failed(format!(
            "Deskline {tag} is out. This copy can't update itself: get it from https://github.com/{REPO}/releases/latest"
        ))));
    }
    let file = dir.join(format!("{tag}-{ASSET}"));
    if !file.exists() {
        std::fs::create_dir_all(dir)?;
        let part = dir.join(format!("{tag}-{ASSET}.{}.part", std::process::id()));
        let mut res = agent().get(format!("https://github.com/{REPO}/releases/download/{tag}/{ASSET}")).call()?;
        std::io::copy(&mut res.body_mut().as_reader(), &mut std::fs::File::create(&part)?)?;
        std::fs::rename(&part, &file)?;
    }
    if let Err(e) = verify(&file, &tag) {
        let _ = std::fs::remove_file(&file);
        return Err(e);
    }
    Ok(Some(Msg::Ready(tag, file)))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(600))).build().into()
}

/// Whether release tag `tag` ("v1.2.3") is a newer version than `current` ("1.2.0"). A tag that isn't
/// a plain version never is, which also keeps it safe to put in a file name.
fn newer(tag: &str, current: &str) -> bool {
    let parse = |v: &str| v.split('.').map(|n| n.parse::<u64>().ok()).collect::<Option<Vec<_>>>();
    match (tag.strip_prefix('v').and_then(parse), parse(current)) {
        (Some(new), Some(cur)) => new > cur,
        _ => false,
    }
}

/// Refuses a download that can't be a working Deskline `tag`: cut short, an error page, or (Windows)
/// an exe that doesn't say it's that version.
fn verify(file: &Path, tag: &str) -> anyhow::Result<()> {
    let size = std::fs::metadata(file)?.len();
    if size <= 1 << 20 {
        bail!("The download of Deskline {tag} is broken ({size} bytes)");
    }
    #[cfg(windows)]
    {
        let want = tag.trim_start_matches('v');
        let said = version_of(file)?;
        if said.split_whitespace().last() != Some(want) {
            bail!("The download of Deskline {tag} says it's version {:?}", said.trim());
        }
    }
    Ok(())
}

/// What `exe --version` prints.
#[cfg(windows)]
fn version_of(exe: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()?;
    // a build that doesn't know --version would open its window (as SYSTEM, in the service)
    for _ in 0..300 {
        if child.try_wait()?.is_some() {
            let mut out = String::new();
            child.stdout.take().context("no output")?.read_to_string(&mut out)?;
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    bail!("The downloaded Deskline doesn't answer --version")
}

/// Our own arguments, minus `--background`: after an update the window comes back.
fn args() -> impl Iterator<Item = std::ffi::OsString> {
    std::env::args_os().skip(1).filter(|a| a != "--background")
}

/// The installed Windows copy in Program Files is its service's to update.
fn service_updates_us() -> bool {
    let (Some(pf), Ok(exe)) = (std::env::var_os("ProgramFiles"), std::env::current_exe()) else { return false };
    let installed = Path::new(&pf).join("Deskline");
    exe.parent().is_some_and(|d| d.to_string_lossy().eq_ignore_ascii_case(&installed.to_string_lossy()))
}

/// Only the .deb's copy can replace itself on Linux (apt does it); others hear about new versions.
fn self_updates() -> bool {
    !cfg!(target_os = "linux")
        || (cfg!(target_arch = "x86_64") && std::env::current_exe().is_ok_and(|e| e == Path::new("/usr/bin/deskline")))
}

#[cfg(windows)]
fn install(new: &Path) -> anyhow::Result<()> {
    swap(&std::env::current_exe()?, new)
}

/// Puts `new` where `exe` is. Windows lets a running exe be renamed but not overwritten, so the old
/// one moves aside to `deskline.old.exe` (`cleanup` deletes it once it has quit).
#[cfg(windows)]
pub(crate) fn swap(exe: &Path, new: &Path) -> anyhow::Result<()> {
    let old = exe.with_file_name(OLD);
    let _ = std::fs::remove_file(&old);
    let cant = || format!("Couldn't replace {}: is its folder read-only?", exe.display());
    std::fs::rename(exe, &old).with_context(cant)?;
    if let Err(e) = std::fs::copy(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).with_context(cant);
    }
    Ok(())
}

/// The installed Windows service's `--server`: when this process is SYSTEM, every few hours update
/// silently and restart the service. SYSTEM runs what lands in `dir`, so only SYSTEM and
/// Administrators may write there (`service::data_dir()`).
#[cfg(windows)]
pub fn run_in_service(dir: PathBuf) {
    if cfg!(debug_assertions) || !crate::service::is_system() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            let updated = check(&dir).and_then(|msg| match msg {
                Some(Msg::Ready(_, file)) => install(&file).map(|()| true),
                _ => Ok(false),
            });
            match updated {
                // the restart ends this process; the new exe starts in its place
                Ok(true) => {
                    if let Err(e) = crate::service::restart_detached() {
                        eprintln!("deskline: updated, but couldn't restart the service: {e:#}");
                    }
                }
                Ok(false) => {}
                Err(e) => eprintln!("deskline: update failed: {e:#}"),
            }
            std::thread::sleep(EVERY);
        }
    });
}

/// Start the newly installed version after `Installed`, with our arguments.
#[cfg(windows)]
pub fn relaunch() {
    // still names this path after the swap, where the new exe now is
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe).args(args()).spawn();
    }
}

/// Call at startup: deletes the exe an update moved aside. Right after an update the old version
/// may still be quitting, so this keeps trying for a minute in the background.
pub fn cleanup() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_file_name(OLD);
        std::thread::spawn(move || {
            for _ in 0..60 {
                if !old.exists() || std::fs::remove_file(&old).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }
}

#[cfg(unix)]
fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    if !cmd.status().with_context(|| format!("Couldn't run {program}"))?.success() {
        bail!("{program} failed");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install(deb: &Path) -> anyhow::Result<()> {
    // polkit asks for the password; apt also brings in any new dependency
    run(Command::new("pkexec").args(["apt-get", "install", "-y"]).arg(std::path::absolute(deb)?))
        .context("Couldn't install the update (was the password prompt cancelled?)")
}

#[cfg(target_os = "linux")]
pub fn relaunch() {
    let _ = Command::new("/usr/bin/deskline").args(args()).spawn();
}

/// The Deskline.app we're running from.
#[cfg(target_os = "macos")]
fn bundle() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .nth(3)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .context("Deskline isn't running from Deskline.app, so it can't update itself")
}

#[cfg(target_os = "macos")]
fn install(dmg: &Path) -> anyhow::Result<()> {
    let app = bundle()?;
    let (mnt, new) = (dmg.with_extension("mnt"), app.with_extension("new"));
    run(Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-mountpoint"]).arg(&mnt).arg(dmg))?;
    let _ = std::fs::remove_dir_all(&new);
    let copied = run(Command::new("ditto").arg(mnt.join("Deskline.app")).arg(&new));
    let _ = Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mnt).status();
    copied?;
    // swap it in whole; the running old version keeps the files it already has open
    std::fs::remove_dir_all(&app).with_context(|| format!("Couldn't replace {}", app.display()))?;
    std::fs::rename(&new, &app)?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn relaunch() {
    if let Ok(app) = bundle() {
        let _ = Command::new("open").arg("-n").arg(app).arg("--args").args(args()).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn only_plain_newer_versions_update() {
        assert!(newer("v0.1.5", "0.1.4"));
        assert!(newer("v0.2.0", "0.1.10"));
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(!newer("v0.1.4", "0.1.4"));
        assert!(!newer("v0.1.3", "0.1.4"));
        assert!(!newer("v0.2.0-beta", "0.1.4"));
        assert!(!newer("v0.2/../../x", "0.1.4"));
        assert!(!newer("0.2.0", "0.1.4"));
    }

    #[cfg(windows)]
    #[test]
    fn swap_moves_the_old_exe_aside() {
        let dir = std::env::temp_dir().join(format!("deskline-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (exe, new) = (dir.join("deskline.exe"), dir.join("v9.9.9-deskline.exe"));
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        std::fs::write(dir.join(super::OLD), "older").unwrap();
        super::swap(&exe, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join(super::OLD)).unwrap(), "old");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
