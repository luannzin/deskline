//! Windows installed mode (STUB: to be implemented).
//!
//! `deskline --install` (elevated) copies the exe to Program Files and registers the "Deskline"
//! service. The service (LocalSystem, session 0) keeps `deskline --server` running as SYSTEM in
//! the active console session, so the sign-in screen, the lock screen and UAC prompts can be
//! seen and controlled, and relays Ctrl+Alt+Del to `SendSAS`.
#![allow(dead_code)]
use std::path::PathBuf;

/// The service is registered on this machine.
pub fn is_installed() -> bool {
    false
}

/// Elevated: copy this exe to `%ProgramFiles%\Deskline`, register and start the service, add
/// the Start menu shortcut, the Apps & features entry, the firewall rule and the SAS policy.
pub fn install() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// Elevated: undo `install`.
pub fn uninstall() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// Start this exe again with `args` through the UAC prompt. Returns once it was launched.
pub fn elevate(_args: &[&str]) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// This process runs elevated (or as SYSTEM).
pub fn is_elevated() -> bool {
    false
}

/// This process runs as LocalSystem (the `--server` started by the service).
pub fn is_system() -> bool {
    false
}

/// `--service`: hand this process to the service control manager. Returns when the service stops.
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// Restart the service from a detached helper (after an update replaced the exe).
pub fn restart_detached() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// `--restart-service`: stop the service, wait, start it again.
pub fn restart_service() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// Ctrl+Alt+Del: asks the service to call `SendSAS` (only a service may).
pub fn request_sas() {}

/// Run `f` as the user signed in to this session (file access for the viewer). Runs `f` directly
/// when this process isn't SYSTEM. Errors when nobody is signed in.
pub fn as_user<T>(f: impl FnOnce() -> T) -> anyhow::Result<T> {
    Ok(f())
}

/// The signed-in user's Downloads folder.
pub fn user_downloads() -> anyhow::Result<PathBuf> {
    anyhow::bail!("not implemented")
}

/// Start Deskline's window as the user signed in to this session, with `args`.
pub fn spawn_ui(_args: &[&str]) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

/// Whether process `pid` runs elevated (for settings changes over the local pipe).
pub fn process_is_elevated(_pid: u32) -> bool {
    false
}

/// The Windows session this process runs in.
pub fn session_id() -> u32 {
    0
}

/// `%ProgramData%\Deskline`, created readable and writable only by SYSTEM and Administrators.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    anyhow::bail!("not implemented")
}
