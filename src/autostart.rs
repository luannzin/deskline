//! Start Deskline (in the background) when the user signs in (STUB: to be implemented).
#![allow(dead_code)]

pub fn enabled() -> bool {
    false
}

/// On: start `<this exe> --background` at sign-in. Off: remove that.
pub fn set(_on: bool) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}
