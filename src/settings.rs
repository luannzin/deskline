//! What Deskline remembers, in `settings.json` next to the identity: the unattended-access
//! password (as an Argon2 hash), who may get in without a click, the address book, and
//! preferences. Plus `sessions.log`, one line per session this computer shared.
//!
//! The installed Windows service keeps its own copy in %ProgramData%\Deskline, which only
//! administrators can read; the window changes its security part through the service.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use serde::{Deserialize, Serialize};

use crate::codec::Quality;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Unattended access: the password's Argon2 hash. None = off.
    pub password: Option<String>,
    /// Viewers who get in, with control, without anyone clicking Accept.
    pub trusted: Vec<Peer>,
    /// People this computer connected to, most recent first.
    pub contacts: Vec<Contact>,
    /// Your own iroh relay server instead of the public ones.
    pub relay: Option<String>,
    pub quality: Quality,
    /// Hear the other computer.
    pub sound: bool,
    /// The Accept prompt's "Let them control" starts ticked.
    pub allow_control: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            password: None,
            trusted: Vec::new(),
            contacts: Vec::new(),
            relay: None,
            quality: Quality::default(),
            sound: true,
            allow_control: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Peer {
    /// The peer's code without spaces (z-base-32 of its key).
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Contact {
    pub id: String,
    /// What their computer calls itself.
    pub name: String,
    /// What you call it.
    pub alias: Option<String>,
    /// Unix seconds of the last session.
    pub last: u64,
}

impl Contact {
    pub fn label(&self) -> &str {
        self.alias.as_deref().filter(|a| !a.trim().is_empty()).unwrap_or(&self.name)
    }
}

/// Settings shared by the window and the network side, saved on every change.
#[derive(Clone)]
pub struct Store {
    path: Arc<PathBuf>,
    inner: Arc<Mutex<Settings>>,
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("settings.json");
        let s = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path: Arc::new(path), inner: Arc::new(Mutex::new(s)) }
    }

    /// The folder this store (and the session log) lives in.
    pub fn dir(&self) -> PathBuf {
        self.path.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    pub fn get(&self) -> Settings {
        self.inner.lock().unwrap().clone()
    }

    /// Change and save. A save that fails keeps the change in memory and says why.
    pub fn update<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> anyhow::Result<R> {
        let mut s = self.inner.lock().unwrap();
        let r = f(&mut s);
        save(&self.path, &s)?;
        Ok(r)
    }

    pub fn set_password(&self, password: Option<&str>) -> anyhow::Result<()> {
        let hash = match password {
            Some(p) => Some(hash(p)?),
            None => None,
        };
        self.update(|s| s.password = hash)
    }

    pub fn trusts(&self, id: &str) -> bool {
        self.inner.lock().unwrap().trusted.iter().any(|p| p.id == id)
    }

    pub fn trust(&self, id: &str, name: &str) -> anyhow::Result<()> {
        self.update(|s| {
            s.trusted.retain(|p| p.id != id);
            s.trusted.push(Peer { id: id.into(), name: name.into() });
        })
    }

    /// Remember a session in the address book (most recent first, at most 100).
    pub fn met(&self, id: &str, name: &str) -> anyhow::Result<()> {
        self.update(|s| {
            let alias = s.contacts.iter().find(|c| c.id == id).and_then(|c| c.alias.clone());
            s.contacts.retain(|c| c.id != id);
            s.contacts.insert(0, Contact { id: id.into(), name: name.into(), alias, last: now() });
            s.contacts.truncate(100);
        })
    }
}

fn save(path: &Path, s: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write then rename, so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    file.open(&tmp)?.write_all(&serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn hash(password: &str) -> anyhow::Result<String> {
    anyhow::ensure!(password.chars().count() >= 8, "Use at least 8 characters.");
    let h = Argon2::default().hash_password(password.as_bytes()).map_err(|e| anyhow::anyhow!("couldn't hash the password: {e}"))?;
    Ok(h.to_string())
}

/// Slow on purpose (Argon2): run it off the async threads.
pub fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// "2026-11-18 14:03 UTC".
pub fn when(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (h, m) = (secs % 86_400 / 3600, secs % 3600 / 60);
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}

/// Add a line to `sessions.log`. Past 1 MB the old log moves to `sessions.old.log`.
pub fn log(dir: &Path, line: &str) {
    let path = dir.join("sessions.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, dir.join("sessions.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}  {line}", when(now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_verify_and_short_ones_are_refused() {
        let h = hash("correct horse").unwrap();
        assert!(h.starts_with("$argon2"));
        assert!(verify("correct horse", &h));
        assert!(!verify("wrong horse", &h));
        assert!(!verify("anything", "not a hash"));
        assert!(hash("short").is_err());
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(when(0), "1970-01-01 00:00 UTC");
        assert_eq!(when(1_795_000_000), "2026-11-18 11:06 UTC");
        assert_eq!(when(951_782_400), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn store_survives_reopen_and_keeps_aliases() {
        let dir = std::env::temp_dir().join(format!("deskline-settings-{}", std::process::id()));
        let store = Store::open(&dir);
        store.met("abc", "pc").unwrap();
        store.update(|s| s.contacts[0].alias = Some("Mom".into())).unwrap();
        store.met("abc", "pc-renamed").unwrap();
        store.trust("xyz", "laptop").unwrap();
        let again = Store::open(&dir).get();
        assert_eq!(again.contacts[0].label(), "Mom");
        assert_eq!(again.contacts[0].name, "pc-renamed");
        assert!(Store::open(&dir).trusts("xyz"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
