//! File transfer. The viewer opens one extra QUIC bidirectional stream per operation: list a
//! folder on the host, upload a file or folder into the host's Downloads, or download a file or
//! folder from the host. The host serves each stream with `serve`.
//!
//! On a stream the viewer sends one `Req` and the host answers with `Reply`s. A tree (upload or
//! download) is first announced as `Item`s, parents before children; then each file's bytes
//! follow in that order as frames: a 4-byte LE length and at most `CHUNK` bytes, an empty frame
//! ending the file, or `SKIPPED` when the sender couldn't read it after all.
//!
//! Each side treats what the other sends as hostile: names are cleaned down to one plain file
//! name, sizes must match what was announced, nothing lands outside the chosen folder, and a
//! transfer that fails or is cancelled leaves nothing behind.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[cfg(windows)]
use crate::service::as_user;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Listing {
    /// The folder listed, as the host spells it. Empty asks for the starting places (home,
    /// Desktop, Downloads, drives).
    pub path: String,
    /// `Some("")`: the starting places.
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

impl Listing {
    /// What to pass to `list` or `download` for one of these entries.
    pub fn path_of(&self, entry: &Entry) -> String {
        if self.path.is_empty() {
            return entry.name.clone();
        }
        // Host paths are absolute: Unix ones start with '/', Windows ones with a drive or \\.
        let sep = if self.path.starts_with('/') { "/" } else { "\\" };
        format!("{}{sep}{}", self.path.strip_suffix(sep).unwrap_or(&self.path), entry.name)
    }
}

/// Bytes done, bytes total.
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// Viewer to host: the first message on a file stream.
#[derive(Serialize, Deserialize)]
enum Req {
    List { path: String },
    /// The tree being sent; the file bytes follow.
    Upload(Vec<Item>),
    Download { path: String },
}

/// Host to viewer.
#[derive(Serialize, Deserialize)]
enum Reply {
    Listing(Listing),
    /// The tree a download sends; the file bytes follow.
    Items(Vec<Item>),
    /// Where an upload landed.
    Done(String),
    Error(String),
}

/// One file or folder of a tree being sent.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Item {
    /// Names from the top of the tree down to this one.
    path: Vec<String>,
    /// `None`: a folder.
    size: Option<u64>,
}

const CHUNK: usize = 64 << 10;
/// In place of a frame length: the sender couldn't read this file after all.
const SKIPPED: u32 = u32::MAX;
const MAX_MSG: usize = 16 << 20;
const LOST: &str = "Lost the connection to their computer";
const ODD: &str = "Their computer sent something Deskline doesn't understand.";

// ---------------------------------------------------------------- viewer

/// Viewer: what's in folder `path` on the host.
pub async fn list(conn: &Connection, path: &str) -> anyhow::Result<Listing> {
    let (mut send, mut recv) = open(conn, &Req::List { path: path.into() }).await?;
    send.finish()?;
    match answer(&mut recv).await? {
        Reply::Listing(l) => Ok(l),
        _ => anyhow::bail!(ODD),
    }
}

/// Viewer: send a local file or folder into the host's Downloads. Returns where it landed.
/// Links and unreadable files inside a folder are left out.
pub async fn upload(conn: &Connection, local: &Path, progress: Progress<'_>) -> anyhow::Result<String> {
    let local = local.to_path_buf();
    let (items, files, _) = blocking(move || walk(&local)).await?;
    let (mut send, mut recv) = open(conn, &Req::Upload(items)).await?;
    // A host that gives up stops reading, which ends this early; its reply says why. Dropping
    // this future ends the stream short of what was announced, so the host gives up too.
    let _ = send_files(&mut send, &files, progress).await;
    let _ = send.finish();
    match answer(&mut recv).await? {
        Reply::Done(at) => Ok(at),
        _ => anyhow::bail!(ODD),
    }
}

/// Viewer: fetch a file or folder from the host into `dest_dir`. Returns the local path.
pub async fn download(conn: &Connection, remote: &str, dest_dir: &Path, progress: Progress<'_>) -> anyhow::Result<PathBuf> {
    let (mut send, mut recv) = open(conn, &Req::Download { path: remote.into() }).await?;
    send.finish()?;
    let Reply::Items(items) = answer(&mut recv).await? else { anyhow::bail!(ODD) };
    Ok(recv_tree(&mut recv, &items, dest_dir, progress).await?.0)
}

async fn open(conn: &Connection, req: &Req) -> anyhow::Result<(SendStream, RecvStream)> {
    let (mut send, recv) = conn.open_bi().await.context(LOST)?;
    send_msg(&mut send, req).await.context(LOST)?;
    Ok((send, recv))
}

/// The host's next reply, its error turned into ours.
async fn answer(recv: &mut RecvStream) -> anyhow::Result<Reply> {
    match recv_msg(recv).await.context(LOST)? {
        Reply::Error(e) => Err(anyhow::anyhow!(e)),
        r => Ok(r),
    }
}

// ---------------------------------------------------------------- host

/// Host: answer one stream the viewer opened. Returns a line for the session log, if any
/// ("Received report.pdf", "Sent Photos"). On an error the viewer is told why, as long as the
/// stream still works.
pub async fn serve(mut send: SendStream, mut recv: RecvStream) -> anyhow::Result<Option<String>> {
    let answered = match recv_msg(&mut recv).await? {
        Req::List { path } => blocking(move || list_dir(&path)).await.map(|l| (Reply::Listing(l), None)),
        Req::Upload(items) => {
            async {
                // Not as the user: when this runs as SYSTEM, finding the signed-in user's
                // Downloads takes SYSTEM's rights.
                let into = tokio::task::spawn_blocking(downloads).await??;
                let (at, skipped) = recv_tree(&mut recv, &items, &into, &quiet).await?;
                let name = at.file_name().unwrap_or_default().to_string_lossy().into_owned();
                anyhow::Ok((Reply::Done(at.display().to_string()), Some(said("Received", &name, skipped))))
            }
            .await
        }
        Req::Download { path } => match blocking(move || walk(&resolve(&path)?)).await {
            Ok((items, files, skipped)) => {
                let name = items[0].path[0].clone();
                match send_msg(&mut send, &Reply::Items(items)).await {
                    Ok(()) => {
                        let skipped = skipped + send_files(&mut send, &files, &quiet).await?;
                        send.finish()?;
                        return Ok(Some(said("Sent", &name, skipped)));
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        },
    };
    let done = match answered {
        Ok((reply, line)) => send_msg(&mut send, &reply).await.map(|()| line),
        Err(e) => Err(e),
    };
    if let Err(e) = &done {
        // Stop reading first, so a viewer still uploading gives up now.
        let _ = recv.stop(0u8.into());
        let _ = send_msg(&mut send, &Reply::Error(format!("{e:#}"))).await;
    }
    let _ = send.finish();
    done
}

fn quiet(_: u64, _: u64) {}

/// "Sent Photos", "Received report (1).pdf".
fn said(what: &str, name: &str, skipped: usize) -> String {
    match skipped {
        0 => format!("{what} {name}"),
        n => format!("{what} {name} ({n} left out: links or unreadable)"),
    }
}

/// The starting places for "", else the folder at `path`: folders first, then by name.
fn list_dir(path: &str) -> anyhow::Result<Listing> {
    let folder = |name: &str| Entry { name: name.into(), dir: true, size: 0 };
    if path.is_empty() {
        let mut entries: Vec<Entry> = places().into_iter().filter(|(_, p)| p.is_dir()).map(|(n, _)| folder(n)).collect();
        entries.extend(roots().iter().map(|r| folder(r.as_str())));
        return Ok(Listing { path: String::new(), parent: None, entries });
    }
    let dir = resolve(path)?;
    let mut entries = Vec::new();
    for e in fs::read_dir(&dir).with_context(|| format!("Couldn't open {}", dir.display()))?.flatten() {
        // Links show what they point to; broken ones are left out.
        let Ok(meta) = e.metadata().and_then(|m| if m.is_symlink() { e.path().metadata() } else { Ok(m) }) else { continue };
        let size = if meta.is_dir() { 0 } else { meta.len() };
        entries.push(Entry { name: e.file_name().to_string_lossy().into_owned(), dir: meta.is_dir(), size });
    }
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    let parent = dir.parent().map_or(String::new(), |p| p.display().to_string());
    Ok(Listing { path: dir.display().to_string(), parent: Some(parent), entries })
}

/// A path from the viewer as a full path here: a starting place's name or an absolute path,
/// with "." and ".." worked out.
fn resolve(path: &str) -> anyhow::Result<PathBuf> {
    if !Path::new(path).is_absolute() {
        let place = places().into_iter().find(|(name, _)| *name == path);
        return place.map(|(_, p)| p).with_context(|| format!("\"{path}\" isn't a full path."));
    }
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            // Drive letters and network shares only: no \\?\ or \\.\ device paths.
            #[cfg(windows)]
            Component::Prefix(p) if !matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::UNC(..)) => {
                anyhow::bail!("\"{path}\" isn't a file or folder path.")
            }
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- trees

/// What sending `root` takes: its tree as items, parents before children, and each file item's
/// local path and size in the same order. Inside a folder, links (they could lead out of it),
/// special files and anything unreadable are left out and counted.
fn walk(root: &Path) -> anyhow::Result<(Vec<Item>, Vec<(PathBuf, u64)>, usize)> {
    let name = root.file_name().with_context(|| format!("Pick a file or folder inside {}.", root.display()))?;
    let name = name.to_string_lossy().into_owned();
    let meta = fs::metadata(root).with_context(|| format!("Couldn't open {}", root.display()))?;
    if meta.is_file() {
        // Fail now rather than after announcing it.
        File::open(root).with_context(|| format!("Couldn't open {}", root.display()))?;
        return Ok((vec![Item { path: vec![name], size: Some(meta.len()) }], vec![(root.into(), meta.len())], 0));
    }
    anyhow::ensure!(meta.is_dir(), "{} isn't a file or folder.", root.display());

    let mut items = vec![Item { path: vec![name.clone()], size: None }];
    let (mut files, mut skipped) = (Vec::new(), 0);
    let mut todo = vec![(root.to_path_buf(), vec![name])];
    while let Some((dir, path)) = todo.pop() {
        let list = match fs::read_dir(&dir) {
            Ok(l) => l,
            Err(e) if dir.as_path() == root => return Err(e).with_context(|| format!("Couldn't open {}", root.display())),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        for e in list {
            // An entry's own metadata: a link is neither a file nor a folder here.
            let Ok((e, meta)) = e.and_then(|e| e.metadata().map(|m| (e, m))) else {
                skipped += 1;
                continue;
            };
            let mut p = path.clone();
            p.push(e.file_name().to_string_lossy().into_owned());
            if meta.is_dir() {
                items.push(Item { path: p.clone(), size: None });
                todo.push((e.path(), p));
            } else if meta.is_file() {
                items.push(Item { path: p, size: Some(meta.len()) });
                files.push((e.path(), meta.len()));
            } else {
                skipped += 1;
            }
        }
    }
    Ok((items, files, skipped))
}

/// Each file's bytes in frames of at most `CHUNK`, then an empty frame; a file that can't be
/// read (or shrank) gets `SKIPPED` instead. Returns how many were skipped.
async fn send_files(send: &mut SendStream, files: &[(PathBuf, u64)], progress: Progress<'_>) -> anyhow::Result<usize> {
    let total = files.iter().map(|f| f.1).fold(0, u64::saturating_add);
    let (mut done, mut skipped) = (0, 0);
    for (path, size) in files {
        let path = path.clone();
        let mut file = blocking(move || Ok(File::open(path)?)).await.ok();
        let mut left = *size;
        let whole = loop {
            let Some(f) = file.take() else { break false };
            if left == 0 {
                break true;
            }
            let n = left.min(CHUNK as u64) as usize;
            let read = blocking(move || {
                let mut data = vec![0; n];
                (&f).read_exact(&mut data)?;
                Ok((f, data))
            });
            let Ok((f, data)) = read.await else { break false };
            send.write_all(&(n as u32).to_le_bytes()).await?;
            send.write_all(&data).await?;
            (file, left, done) = (Some(f), left - n as u64, done + n as u64);
            progress(done, total);
        };
        if whole {
            send.write_all(&0u32.to_le_bytes()).await?;
        } else {
            send.write_all(&SKIPPED.to_le_bytes()).await?;
            (skipped, done) = (skipped + 1, done + left);
            progress(done, total);
        }
    }
    Ok(skipped)
}

/// Receive the tree `items` announces into `into`, as a new file or folder named after its top
/// (" (1)"… when taken). Names are cleaned and sizes held to what was announced. It's written
/// as "<name>.part", renamed once complete, and deleted on any error or if this future is
/// dropped. Returns where it landed and how many files the sender left out.
async fn recv_tree(recv: &mut RecvStream, items: &[Item], into: &Path, progress: Progress<'_>) -> anyhow::Result<(PathBuf, usize)> {
    let Some((top, rest)) = items.split_first() else { anyhow::bail!(ODD) };
    anyhow::ensure!(top.path.len() == 1 && (top.size.is_none() || rest.is_empty()), ODD);
    let total = items.iter().try_fold(0u64, |t, i| t.checked_add(i.size.unwrap_or(0))).context(ODD)?;
    let (mut done, mut skipped) = (0, 0);

    let (dir, name, folder) = (into.to_path_buf(), clean(&top.path[0]), top.size.is_none());
    let made = {
        let name = name.clone();
        blocking(move || create(&dir, &name, folder, true)).await?
    };
    let part = made.0.clone();
    let mut partial = Partial(Some(part.clone()));
    let mut folders = HashMap::new();
    match (top.size, made.1) {
        (Some(size), Some(file)) => {
            if !recv_file(recv, file, size, &name, total, &mut done, progress).await? {
                anyhow::bail!("{name} became unreadable while it was being sent.");
            }
        }
        _ => {
            folders.insert(top.path.clone(), part.clone());
        }
    }
    for item in rest {
        let (last, parent) = item.path.split_last().context(ODD)?;
        let dir = folders.get(parent).context(ODD)?.clone();
        let (name, folder) = (clean(last), item.size.is_none());
        let (path, file) = {
            let name = name.clone();
            blocking(move || create(&dir, &name, folder, false)).await?
        };
        match (item.size, file) {
            (Some(size), Some(file)) => {
                if !recv_file(recv, file, size, &name, total, &mut done, progress).await? {
                    skipped += 1;
                    let _ = blocking(move || Ok(fs::remove_file(path)?)).await;
                }
            }
            _ => {
                folders.insert(item.path.clone(), path);
            }
        }
    }
    let at = blocking(move || finish(&part)).await?;
    partial.0 = None;
    Ok((at, skipped))
}

/// One file's frames into `file`. False: the sender couldn't read it after all.
async fn recv_file(
    recv: &mut RecvStream,
    mut file: File,
    size: u64,
    name: &str,
    total: u64,
    done: &mut u64,
    progress: Progress<'_>,
) -> anyhow::Result<bool> {
    let mut got = 0;
    loop {
        let mut len = [0; 4];
        recv.read_exact(&mut len).await.context(LOST)?;
        let n = match u32::from_le_bytes(len) {
            0 if got == size => return Ok(true),
            SKIPPED => {
                *done += size - got;
                progress(*done, total);
                return Ok(false);
            }
            n if n != 0 && n as usize <= CHUNK && got + n as u64 <= size => n as usize,
            _ => anyhow::bail!("{name} doesn't match the size it was announced with."),
        };
        let mut data = vec![0; n];
        recv.read_exact(&mut data).await.context(LOST)?;
        let write = blocking(move || {
            (&file).write_all(&data)?;
            Ok(file)
        });
        file = write.await.with_context(|| format!("Couldn't save {name}"))?;
        got += n as u64;
        *done += n as u64;
        progress(*done, total);
    }
}

/// Make a new file or a folder in `dir` named `name`, or "name (1)"… when that's taken. With
/// `part` it's made as "<name>.part", and the plain name must be free too.
fn create(dir: &Path, name: &str, folder: bool, part: bool) -> anyhow::Result<(PathBuf, Option<File>)> {
    for v in variants(name, !folder) {
        let path = dir.join(&v);
        let made = if part { dir.join(format!("{v}.part")) } else { path.clone() };
        if taken(&path) || taken(&made) {
            continue;
        }
        let res = if folder { fs::create_dir(&made).map(|()| None) } else { File::create_new(&made).map(Some) };
        match res {
            Ok(f) => return Ok((made, f)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("Couldn't create {}", made.display())),
        }
    }
    anyhow::bail!("There are too many files called {name} already.")
}

/// Give a complete "<name>.part" its name, or the next free one if that got taken meanwhile.
fn finish(part: &Path) -> anyhow::Result<PathBuf> {
    let name = part.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let name = name.strip_suffix(".part").unwrap_or(&name);
    let dir = part.parent().unwrap_or(part);
    let at = variants(name, part.is_file()).map(|v| dir.join(v)).find(|p| !taken(p));
    let at = at.with_context(|| format!("There are too many files called {name} already."))?;
    fs::rename(part, &at).with_context(|| format!("Couldn't rename {}", part.display()))?;
    Ok(at)
}

/// "name", then "name (1).ext", "name (2).ext"…
fn variants(name: &str, file: bool) -> impl Iterator<Item = String> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if file && i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((1..10_000).map(move |n| format!("{stem} ({n}){ext}")))
}

fn taken(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// A name from the other side made safe as one plain file name on any system: no separators,
/// drive letters or "..", nothing Windows reserves or silently changes, at most 200 bytes (room
/// for " (12)" and ".part" under every system's limit).
fn clean(name: &str) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let mut end = s.len().min(200);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    // Windows drops trailing dots and spaces, so "x." would quietly become "x".
    let s = s.trim_end_matches(['.', ' ']);
    // Device names stay devices whatever the extension: "nul.txt" is NUL.
    let stem = s.split('.').next().unwrap_or_default().trim_end().to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|p| stem.strip_prefix(p).is_some_and(|n| n.chars().count() == 1 && "0123456789¹²³".contains(n)));
    match s {
        "" => "_".into(),
        s if reserved => format!("_{s}"),
        s => s.into(),
    }
}

/// Deletes a half-received file or folder unless defused: on errors, and when the transfer's
/// future is dropped.
struct Partial(Option<PathBuf>);

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = as_user(|| if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) });
        }
    }
}

/// File work, off the async threads and as the signed-in user (`service::as_user`).
/// Impersonation is per thread, hence one call per piece of work.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(move || as_user(f)).await??
}

/// Elsewhere the host already runs as the signed-in user.
#[cfg(not(windows))]
fn as_user<T>(f: impl FnOnce() -> T) -> anyhow::Result<T> {
    Ok(f())
}

// ---------------------------------------------------------------- places

/// This user's Downloads folder (on the host: the signed-in user's).
pub fn downloads() -> anyhow::Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = tests::DOWNLOADS.get() {
        return Ok(dir.clone());
    }
    #[cfg(windows)]
    let dir = if crate::service::is_system() {
        crate::service::user_downloads()?
    } else {
        known_folder(&windows::Win32::UI::Shell::FOLDERID_Downloads).or_else(|_| {
            let profile = std::env::var_os("USERPROFILE").context("Couldn't find your Downloads folder.")?;
            anyhow::Ok(PathBuf::from(profile).join("Downloads"))
        })?
    };
    #[cfg(not(windows))]
    let dir = user_dir("XDG_DOWNLOAD_DIR", "Downloads").context("Couldn't find your Downloads folder.")?;
    fs::create_dir_all(&dir).with_context(|| format!("Couldn't create {}", dir.display()))?;
    Ok(dir)
}

/// Home, Desktop, Documents and Downloads, by the names the viewer sees.
#[cfg(windows)]
fn places() -> Vec<(&'static str, PathBuf)> {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Profile};
    [("Home", FOLDERID_Profile), ("Desktop", FOLDERID_Desktop), ("Documents", FOLDERID_Documents), ("Downloads", FOLDERID_Downloads)]
        .into_iter()
        .filter_map(|(name, id)| Some((name, known_folder(&id).ok()?)))
        .collect()
}

/// Home, Desktop, Documents and Downloads, by the names the viewer sees.
#[cfg(not(windows))]
fn places() -> Vec<(&'static str, PathBuf)> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| ("Home", PathBuf::from(h)));
    let dirs = [("Desktop", "XDG_DESKTOP_DIR"), ("Documents", "XDG_DOCUMENTS_DIR"), ("Downloads", "XDG_DOWNLOAD_DIR")];
    home.into_iter().chain(dirs.into_iter().filter_map(|(name, key)| Some((name, user_dir(key, name)?)))).collect()
}

/// Drive roots, or "/".
fn roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let drives = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
        (0..26u8).filter(|i| drives & (1 << i) != 0).map(|i| format!("{}:\\", (b'A' + i) as char)).collect()
    }
    #[cfg(not(windows))]
    vec!["/".into()]
}

/// A shell folder (Desktop, Documents…) of the user this thread acts as.
#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> anyhow::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{TOKEN_IMPERSONATE, TOKEN_QUERY};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        // Without a token the shell answers for the process's user, SYSTEM under the service:
        // pass the impersonation token when there is one.
        let mut h = HANDLE::default();
        let token = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY | TOKEN_IMPERSONATE, true, &mut h).ok().map(|()| h);
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, token);
        if let Some(t) = token {
            let _ = CloseHandle(t);
        }
        let path = path?;
        let dir = PathBuf::from(std::ffi::OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0 as *const _));
        Ok(dir)
    }
}

/// A folder in the home folder. On Linux, where ~/.config/user-dirs.dirs says: desktops move
/// and translate them (XDG_DOWNLOAD_DIR="$HOME/Téléchargements").
#[cfg(not(windows))]
fn user_dir(key: &str, fallback: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?);
    let config = std::env::var_os("XDG_CONFIG_HOME").filter(|c| !c.is_empty()).map_or_else(|| home.join(".config"), PathBuf::from);
    let dirs = if cfg!(target_os = "linux") { fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default() } else { String::new() };
    let set = dirs.lines().find_map(|l| l.trim().strip_prefix(key)?.trim_start().strip_prefix('=')).map(|v| v.trim().trim_matches('"'));
    Some(home.join(match set {
        Some(v) => v.strip_prefix("$HOME").map_or(v, |rest| rest.trim_start_matches('/')),
        None => fallback,
    }))
}

// ---------------------------------------------------------------- wire

async fn send_msg<T: Serialize>(w: &mut SendStream, m: &T) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(m)?;
    anyhow::ensure!(bytes.len() <= MAX_MSG, "That's too many files at once.");
    w.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    Ok(())
}

async fn recv_msg<T: DeserializeOwned>(r: &mut RecvStream) -> anyhow::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(n <= MAX_MSG, "message too large: {n} bytes");
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use iroh::endpoint::presets;
    use iroh::{Endpoint, RelayMode};

    /// Host uploads land here instead of the real Downloads.
    pub(super) static DOWNLOADS: OnceLock<PathBuf> = OnceLock::new();

    #[test]
    fn hostile_names_become_one_plain_name() {
        let cases = [
            ("../../evil", ".._.._evil"),
            ("C:\\x", "C__x"),
            ("CON", "_CON"),
            ("nul.txt", "_nul.txt"),
            ("COM1", "_COM1"),
            ("COM10", "COM10"),
            ("a/b", "a_b"),
            ("..", "_"),
            ("", "_"),
            ("x. .", "x"),
            ("a\0\nb", "ab"),
            ("report.pdf", "report.pdf"),
        ];
        for (raw, want) in cases {
            let got = clean(raw);
            assert_eq!(got, want, "{raw:?}");
            assert!(matches!(Path::new(&got).components().collect::<Vec<_>>()[..], [Component::Normal(_)]), "{raw:?}");
        }
        assert!(clean(&"é".repeat(300)).len() <= 200);
        assert_eq!(variants("report.pdf", true).take(3).collect::<Vec<_>>(), ["report.pdf", "report (1).pdf", "report (2).pdf"]);
        assert_eq!(variants(".bashrc", true).nth(1).unwrap(), ".bashrc (1)");
        assert_eq!(variants("my.photos", false).nth(1).unwrap(), "my.photos (1)");
    }

    /// Two real endpoints on this machine, dialed directly: no relay, no lookups. The host
    /// serves every bidirectional stream as a file operation.
    async fn pair() -> (Endpoint, Connection) {
        let alpn = b"deskline-files-test".to_vec();
        let ep = || Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).alpns(vec![alpn.clone()]).bind();
        let (host, viewer) = (ep().await.unwrap(), ep().await.unwrap());
        let addr = host.addr();
        tokio::spawn(async move {
            let conn = host.accept().await.unwrap().await.unwrap();
            while let Ok((send, recv)) = conn.accept_bi().await {
                tokio::spawn(serve(send, recv));
            }
        });
        let conn = viewer.connect(addr, &alpn).await.unwrap();
        (viewer, conn)
    }

    /// Every file and folder under `dir`: relative path and contents (`None` for a folder).
    fn tree(dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let name = PathBuf::from(p.file_name().unwrap());
            if p.is_dir() {
                out.extend(tree(&p).into_iter().map(|(q, c)| (name.join(q), c)));
                out.push((name, None));
            } else {
                out.push((name, Some(fs::read(&p).unwrap())));
            }
        }
        out.sort();
        out
    }

    /// An upload the way a hostile viewer might send it.
    async fn raw(conn: &Connection, items: Vec<Item>, data: &[u8]) -> anyhow::Result<String> {
        let (mut send, mut recv) = open(conn, &Req::Upload(items)).await?;
        let _ = send.write_all(data).await;
        let _ = send.finish();
        match answer(&mut recv).await? {
            Reply::Done(at) => Ok(at),
            _ => anyhow::bail!(ODD),
        }
    }

    fn frame(data: &[u8]) -> Vec<u8> {
        [&(data.len() as u32).to_le_bytes()[..], data].concat()
    }

    fn item(path: &[&str], size: Option<u64>) -> Item {
        Item { path: path.iter().map(|s| s.to_string()).collect(), size }
    }

    async fn wait(what: &str, ok: impl Fn() -> bool) {
        let poll = async {
            while !ok() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(20), poll).await.unwrap_or_else(|_| panic!("never saw {what}"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_upload_download_and_hostile_viewers() {
        let root = std::env::temp_dir().join(format!("deskline-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (host_dl, mine, got) = (root.join("host-downloads"), root.join("mine"), root.join("got"));
        for d in [&host_dl, &mine.join("Photos").join("sub"), &mine.join("Photos").join("empty"), &got] {
            fs::create_dir_all(d).unwrap();
        }
        DOWNLOADS.set(host_dl.clone()).unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs::write(mine.join("report.pdf"), &big).unwrap();
        fs::write(mine.join("Photos").join("a.txt"), b"hello").unwrap();
        fs::write(mine.join("Photos").join("sub").join("b.bin"), &big[..70_000]).unwrap();
        let (_viewer, conn) = pair().await;
        let last = Mutex::new((0u64, 0u64));
        let progress = |d: u64, t: u64| *last.lock().unwrap() = (d, t);
        let s = |p: &Path| p.display().to_string();
        let at_home = |at: &str| Path::new(at).parent() == Some(host_dl.as_path());

        // Listing: the starting places, then a folder, folders first.
        let places = list(&conn, "").await.unwrap();
        assert!(places.parent.is_none() && !places.entries.is_empty(), "{places:?}");
        let l = list(&conn, &s(&mine)).await.unwrap();
        assert_eq!(l.entries.iter().map(|e| (e.name.as_str(), e.dir)).collect::<Vec<_>>(), [("Photos", true), ("report.pdf", false)]);
        assert_eq!(l.entries[1].size, 200_000);
        assert_eq!(l.parent, Some(s(&root)));
        assert_eq!(list(&conn, &l.path_of(&l.entries[0])).await.unwrap().entries.len(), 3);
        assert!(list(&conn, "somewhere/relative").await.is_err());

        // Uploads land in Downloads; the same name again gets " (1)".
        let at = upload(&conn, &mine.join("report.pdf"), &progress).await.unwrap();
        assert_eq!(PathBuf::from(&at), host_dl.join("report.pdf"));
        assert_eq!(*last.lock().unwrap(), (200_000, 200_000));
        let again = upload(&conn, &mine.join("report.pdf"), &progress).await.unwrap();
        assert_eq!(PathBuf::from(&again), host_dl.join("report (1).pdf"));
        assert_eq!(fs::read(&again).unwrap(), big);
        let photos = upload(&conn, &mine.join("Photos"), &progress).await.unwrap();
        assert_eq!(tree(Path::new(&photos)), tree(&mine.join("Photos")));

        // Downloads back, same rules on this side.
        let p = download(&conn, &photos, &got, &progress).await.unwrap();
        assert_eq!(p, got.join("Photos"));
        assert_eq!(tree(&p), tree(&mine.join("Photos")));
        assert_eq!(download(&conn, &photos, &got, &progress).await.unwrap(), got.join("Photos (1)"));
        assert_eq!(fs::read(download(&conn, &at, &got, &progress).await.unwrap()).unwrap(), big);
        let e = download(&conn, &s(&host_dl.join("missing")), &got, &progress).await.unwrap_err();
        assert!(e.to_string().contains("Couldn't open"), "{e}");

        // A hostile viewer: names never leave Downloads...
        let at = raw(&conn, vec![item(&["../../evil"], Some(3))], &[frame(b"bad"), frame(b"")].concat()).await.unwrap();
        assert!(at_home(&at), "{at}");
        let items = vec![
            item(&["C:\\x"], None),
            item(&["C:\\x", "CON"], Some(1)),
            item(&["C:\\x", ".."], None),
            item(&["C:\\x", "..", "a/b"], Some(1)),
        ];
        let at = raw(&conn, items, &[frame(b"1"), frame(b""), frame(b"2"), frame(b"")].concat()).await.unwrap();
        assert!(at_home(&at), "{at}");
        let mut want = vec![
            (PathBuf::from("_"), None),
            (Path::new("_").join("a_b"), Some(b"2".to_vec())),
            (PathBuf::from("_CON"), Some(b"1".to_vec())),
        ];
        want.sort();
        assert_eq!(tree(Path::new(&at)), want);
        let e = raw(&conn, vec![item(&["t"], None), item(&["elsewhere", "x"], Some(1))], &frame(b"")).await.unwrap_err();
        assert_eq!(e.to_string(), ODD);

        // ...sizes must be what was announced, and a failed upload leaves nothing.
        for data in [[frame(&[1; 20]), frame(b"")].concat(), [frame(&[1; 5]), frame(b"")].concat(), frame(&vec![1; CHUNK + 1])] {
            let e = raw(&conn, vec![item(&["liar"], Some(10))], &data).await.unwrap_err();
            assert!(e.to_string().contains("announced"), "{e}");
        }

        // A viewer that goes away mid-upload leaves nothing either, nor does a dropped download.
        fs::write(mine.join("big.bin"), vec![7; 4 << 20]).unwrap();
        let slow = |_: u64, _: u64| std::thread::sleep(Duration::from_millis(5));
        let (part, local, remote) = (host_dl.join("big.bin.part"), mine.join("big.bin"), s(&mine.join("big.bin")));
        let up = Box::pin(upload(&conn, &local, &slow));
        tokio::select! {
            _ = up => panic!("the upload finished before it could be cancelled"),
            _ = wait("the host's .part", || part.exists()) => {}
        }
        wait("the host cleaning up", || !part.exists()).await;
        let mine_part = got.join("big.bin.part");
        let down = Box::pin(download(&conn, &remote, &got, &slow));
        tokio::select! {
            _ = down => panic!("the download finished before it could be cancelled"),
            _ = wait("the viewer's .part", || mine_part.exists()) => {}
        }
        assert!(!mine_part.exists());

        // Nothing anywhere but what was asked for.
        let mut beside: Vec<_> = fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name()).collect();
        beside.sort();
        assert_eq!(beside, ["got", "host-downloads", "mine"]);
        let mut landed: Vec<_> = fs::read_dir(&host_dl).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        landed.sort();
        assert_eq!(landed, [".._.._evil", "C__x", "Photos", "report (1).pdf", "report.pdf"]);
        conn.close(0u8.into(), b"bye");
        let _ = fs::remove_dir_all(&root);
    }
}
