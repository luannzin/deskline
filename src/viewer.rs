//! Watching someone else's screen: connect, get in (their click, their password or their
//! trust), show the picture, send input, and the extras: sound, clipboard, chat, files. A link
//! that drops reconnects on its own for a minute.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage};
use iroh::endpoint::{Connection, ConnectionError, RecvStream, VarInt};
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio::time::timeout;

use crate::codec::{self, Quality};
use crate::desktop::{CTRL, Input, Monitor, V};
use crate::files::Dest;
use crate::proto::{self, ALPN, Ask, CONTROL_PRIORITY, Hello, Packet, STOPPED, Tell, VERSION, recv_msg, send_msg};
use crate::{audio, clip, files, net};

/// Files copied on their computer come over to this one's clipboard by themselves up to this
/// many bytes; past it the window offers to (`ViewEvent::TheirFiles`).
const AUTO_COPY: u64 = 256 << 20;

/// What a viewing session tells the window.
#[derive(Debug)]
pub enum ViewEvent {
    Status(String),
    Viewing { who: String, control: bool },
    Control(bool),
    Monitors { list: Vec<Monitor>, current: usize },
    /// The path in use ("LAN", "Direct", "Relay"), its round trip in ms, and pictures shown in
    /// the last second.
    Link { kind: &'static str, ms: u32, fps: u32 },
    Chat(String),
    Note(String),
    Privacy(bool),
    Listing(files::Listing),
    Transfer(Transfer),
    /// They copied files too big to come over unasked (0: not any more); `Cmd::CopyTheirs`.
    TheirFiles { count: usize, bytes: u64 },
    /// Their computer takes files into any folder and on its clipboard.
    FileTools,
    /// `password`: their unattended-access password would get you in.
    Ended { why: String, password: bool },
}

/// A file transfer, under way or over.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// This session's own number for it, for `Cmd::Cancel`.
    pub id: u64,
    pub what: What,
    pub done: u64,
    pub total: u64,
    /// Once it's over: where it landed (on this computer, for what came here), or why not.
    pub end: Option<Result<String, String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum What {
    /// A file or folder, by name.
    Send(String),
    Get(String),
    /// This many files pasted onto their computer from this one's clipboard.
    PasteThere(usize),
    /// This many files copied on their computer, coming to this one's clipboard.
    PasteHere(usize),
}

/// The window's side of a session.
pub enum Cmd {
    Ask(Ask),
    /// List a folder on their computer ("" for the starting places).
    List(String),
    /// Send a file or folder: into this folder of theirs, or their Downloads.
    Upload(PathBuf, Option<String>),
    /// Fetch a file or folder of theirs into this computer's Downloads.
    Download(String),
    /// Stop that transfer.
    Cancel(u64),
    /// Ctrl+V on their screen: files on this computer's clipboard go over first.
    Paste,
    /// Copy the files on their clipboard here after all (`ViewEvent::TheirFiles`).
    CopyTheirs,
    /// From the session's own tasks: put these files on this computer's clipboard.
    Clipboard(Vec<PathBuf>),
    Bye,
}

#[derive(Clone)]
pub struct Options {
    pub name: String,
    pub password: Option<String>,
    pub remember: bool,
    pub sound: bool,
    pub quality: Quality,
}

/// Where sessions send events; wakes the window when it has one.
#[derive(Clone)]
pub struct Out {
    tx: mpsc::UnboundedSender<(u64, ViewEvent)>,
    ctx: Option<egui::Context>,
}

impl Out {
    pub fn new(tx: mpsc::UnboundedSender<(u64, ViewEvent)>, ctx: Option<egui::Context>) -> Self {
        Self { tx, ctx }
    }

    fn send(&self, id: u64, e: ViewEvent) {
        let _ = self.tx.send((id, e));
        self.repaint();
    }

    fn repaint(&self) {
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }
}

/// A session to someone else's screen. Dropping it hangs up.
pub struct View {
    pub id: u64,
    tx: mpsc::UnboundedSender<Cmd>,
    /// The newest decoded picture, taken by the window when it draws.
    pub frame: Arc<Mutex<Option<ColorImage>>>,
}

impl View {
    pub fn send(&self, a: Ask) {
        self.cmd(Cmd::Ask(a));
    }

    pub fn cmd(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Bye);
    }
}

static IDS: AtomicU64 = AtomicU64::new(1);

pub fn view(ep: Endpoint, peer: EndpointId, opts: Options, out: Out) -> View {
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel();
    let frame = Arc::new(Mutex::new(None));
    let (acks, slot) = (tx.clone(), frame.clone());
    tokio::spawn(async move {
        let (why, password) = run(ep, peer, opts, rx, acks, slot, &out, id).await;
        out.send(id, ViewEvent::Ended { why, password });
    });
    View { id, tx, frame }
}

enum Outcome {
    /// Over for good, with why ("" when you hung up).
    Done(String),
    Denied { why: String, password: bool },
    /// The link died; `got_in` says whether the session had started.
    Lost { why: String, got_in: bool },
}

/// Sessions until one ends for good; a dropped link is redialed (asking to resume) for up to a
/// minute.
#[allow(clippy::too_many_arguments)]
async fn run(
    ep: Endpoint,
    peer: EndpointId,
    opts: Options,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    acks: mpsc::UnboundedSender<Cmd>,
    slot: Arc<Mutex<Option<ColorImage>>>,
    out: &Out,
    id: u64,
) -> (String, bool) {
    let mut resume = false;
    let mut lost: Option<Instant> = None;
    let mut delay = Duration::from_secs(1);
    loop {
        match session(&ep, peer, &opts, resume, &mut rx, &acks, &slot, out, id).await {
            Outcome::Done(why) => return (why, false),
            Outcome::Denied { why, password } => return (why, password),
            Outcome::Lost { why, got_in } => {
                if got_in {
                    (lost, delay) = (None, Duration::from_secs(1));
                } else if !resume {
                    return (why, false);
                }
                if lost.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(60) {
                    return ("Connection lost.".into(), false);
                }
                out.send(id, ViewEvent::Status("Connection lost. Reconnecting…".into()));
                // Waiting also drains acks the old session left queued: they'd confuse the next.
                if until_bye(&mut rx, tokio::time::sleep(delay)).await.is_none() {
                    return (String::new(), false);
                }
                delay = (delay * 2).min(Duration::from_secs(5));
                resume = true;
            }
        }
    }
}

/// Run `f` unless the window hangs up first (None). Other commands meanwhile are dropped.
async fn until_bye<F: Future>(rx: &mut mpsc::UnboundedReceiver<Cmd>, f: F) -> Option<F::Output> {
    tokio::pin!(f);
    loop {
        tokio::select! {
            r = &mut f => return Some(r),
            c = rx.recv() => if matches!(c, None | Some(Cmd::Bye)) {
                return None;
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn session(
    ep: &Endpoint,
    peer: EndpointId,
    opts: &Options,
    resume: bool,
    rx: &mut mpsc::UnboundedReceiver<Cmd>,
    acks: &mpsc::UnboundedSender<Cmd>,
    slot: &Arc<Mutex<Option<ColorImage>>>,
    out: &Out,
    id: u64,
) -> Outcome {
    let lost = |why: String| Outcome::Lost { why, got_in: false };
    if !resume {
        out.send(id, ViewEvent::Status("Reaching their computer…".into()));
    }
    let conn = match until_bye(rx, timeout(Duration::from_secs(20), ep.connect(peer, ALPN))).await {
        None => return Outcome::Done(String::new()),
        Some(Err(_)) => return lost("Couldn't reach that computer. Is Deskline open there?".into()),
        Some(Ok(Err(e))) => return lost(connect_error(&e.to_string())),
        Some(Ok(Ok(c))) => c,
    };
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => return lost(e.to_string()),
    };
    // Acks and input go before file uploads.
    let _ = send.set_priority(CONTROL_PRIORITY);
    let hello = Hello { version: VERSION.into(), name: opts.name.clone(), password: opts.password.clone(), remember: opts.remember, resume };
    if let Err(e) = send_msg(&mut send, &hello).await {
        return lost(e.to_string());
    }
    if !resume {
        out.send(id, ViewEvent::Status("Waiting for them to accept…".into()));
    }
    let reply = match until_bye(rx, recv_msg::<Tell, _>(&mut recv)).await {
        None => {
            conn.close(0u8.into(), b"bye");
            return Outcome::Done(String::new());
        }
        Some(Err(_)) => return lost("They closed the connection.".into()),
        Some(Ok(t)) => t,
    };
    let mut control = match reply {
        Tell::Accepted { name, control } => {
            out.send(id, ViewEvent::Viewing { who: proto::clean(&name), control });
            control
        }
        Tell::Denied { why, password } => return Outcome::Denied { why, password },
        _ => return Outcome::Done("Unexpected reply.".into()),
    };
    let mut asks = vec![Ask::Sound(opts.sound)];
    if opts.quality != Quality::default() {
        asks.push(Ask::Quality(opts.quality));
    }
    for a in &asks {
        let _ = send_msg(&mut send, a).await;
    }

    let video = match conn.accept_uni().await {
        Ok(v) => v,
        Err(e) => return Outcome::Lost { why: e.to_string(), got_in: true },
    };
    let shown = Arc::new(AtomicU32::new(0));
    let decoder = tokio::spawn(decode(video, acks.clone(), slot.clone(), out.clone(), shown.clone()));
    let sound = tokio::spawn(play(conn.clone(), out.clone(), id));
    let mut tells = proto::reader::<Tell>(recv);
    let mut clipboard = if control { clip::Clipboard::watch().ok() } else { None };
    // File operations, each returning its transfer number (0 for a listing).
    let mut transfers = tokio::task::JoinSet::new();
    let mut running: HashMap<u64, AbortHandle> = HashMap::new();
    let mut numbers = 0u64;
    let mut number = || {
        numbers += 1;
        numbers
    };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // Their computer takes files into any folder and on its clipboard.
    let mut tools = false;
    // Files put on this computer's clipboard from theirs; theirs, too big to copy unasked.
    let (mut ours, mut theirs) = (Vec::<PathBuf>::new(), None::<(Vec<String>, u64)>);

    let outcome = loop {
        tokio::select! {
            c = rx.recv() => match c {
                Some(Cmd::Ask(a)) => {
                    if send_msg(&mut send, &a).await.is_err() {
                        break Outcome::Lost { why: "Connection lost.".into(), got_in: true };
                    }
                }
                Some(Cmd::List(path)) => {
                    let (conn, out) = (conn.clone(), out.clone());
                    transfers.spawn(async move {
                        match files::list(&conn, &path).await {
                            Ok(l) => out.send(id, ViewEvent::Listing(l)),
                            Err(e) => out.send(id, ViewEvent::Note(format!("Couldn't open that folder: {e}"))),
                        }
                        0
                    });
                }
                Some(Cmd::Upload(path, to)) => {
                    let n = number();
                    let r = Report::new(out, id, n, What::Send(file_name(&path)));
                    running.insert(n, transfers.spawn(upload(conn.clone(), path, to.map(Dest::Folder), r)));
                }
                Some(Cmd::Download(remote)) => {
                    let n = number();
                    let r = Report::new(out, id, n, What::Get(file_name(std::path::Path::new(&remote))));
                    running.insert(n, transfers.spawn(download(conn.clone(), remote, r)));
                }
                Some(Cmd::Cancel(n)) => {
                    if let Some(t) = running.remove(&n) {
                        t.abort();
                    }
                }
                Some(Cmd::Paste) => {
                    let mine = match &clipboard {
                        Some(c) if tools => c.files().await,
                        _ => Vec::new(),
                    };
                    if !mine.is_empty() && mine != ours {
                        let n = number();
                        let r = Report::new(out, id, n, What::PasteThere(mine.len()));
                        running.insert(n, transfers.spawn(paste_there(conn.clone(), mine, acks.clone(), r)));
                    } else {
                        // A paste of what's already there (text, or files that came from it):
                        // Ctrl is down already, since it made this a paste.
                        for down in [true, false] {
                            let _ = send_msg(&mut send, &Ask::Input(Input::Key { code: V, down })).await;
                        }
                    }
                }
                Some(Cmd::CopyTheirs) => {
                    if let Some((paths, bytes)) = theirs.take() {
                        let n = number();
                        let r = Report::new(out, id, n, What::PasteHere(paths.len()));
                        running.insert(n, transfers.spawn(copy_here(conn.clone(), paths, bytes, acks.clone(), r)));
                    }
                }
                Some(Cmd::Clipboard(paths)) => {
                    if let Some(c) = &clipboard
                        && c.set_files(paths.clone()).await
                    {
                        ours = paths;
                    }
                }
                Some(Cmd::Bye) | None => {
                    conn.close(0u8.into(), b"bye");
                    break Outcome::Done(String::new());
                }
            },
            t = tells.recv() => match t {
                Some(Tell::Control(on)) => {
                    if on != control {
                        control = on;
                        clipboard = if on { clip::Clipboard::watch().ok() } else { None };
                    }
                    out.send(id, ViewEvent::Control(on));
                }
                Some(Tell::Monitors { list, current }) => out.send(id, ViewEvent::Monitors { list, current }),
                Some(Tell::Clip(text)) => {
                    if let Some(c) = &clipboard {
                        c.set(text);
                    }
                    if theirs.take().is_some() {
                        out.send(id, ViewEvent::TheirFiles { count: 0, bytes: 0 });
                    }
                }
                Some(Tell::FileTools) => {
                    tools = true;
                    out.send(id, ViewEvent::FileTools);
                }
                // Files copied over there come to this clipboard: by themselves when small.
                Some(Tell::ClipFiles { paths, bytes }) if clipboard.is_some() && !paths.is_empty() => {
                    if bytes <= AUTO_COPY {
                        theirs = None;
                        let n = number();
                        let r = Report::new(out, id, n, What::PasteHere(paths.len()));
                        running.insert(n, transfers.spawn(copy_here(conn.clone(), paths, bytes, acks.clone(), r)));
                    } else {
                        out.send(id, ViewEvent::TheirFiles { count: paths.len(), bytes });
                        theirs = Some((paths, bytes));
                    }
                }
                Some(Tell::Chat(text)) => out.send(id, ViewEvent::Chat(proto::clean_chat(&text))),
                Some(Tell::Privacy(on)) => out.send(id, ViewEvent::Privacy(on)),
                Some(Tell::Note(text)) => out.send(id, ViewEvent::Note(proto::clean_chat(&text))),
                Some(_) => {}
                None => {
                    // The stream can end a moment before the close reason is known.
                    let reason = match conn.close_reason() {
                        Some(r) => Some(r),
                        None => timeout(Duration::from_secs(2), conn.closed()).await.ok(),
                    };
                    break match reason {
                        Some(ConnectionError::ApplicationClosed(c)) if c.error_code == VarInt::from_u32(STOPPED) => {
                            Outcome::Done("They stopped sharing.".into())
                        }
                        Some(ConnectionError::ApplicationClosed(_)) => Outcome::Done("The session ended.".into()),
                        _ => Outcome::Lost { why: "Connection lost.".into(), got_in: true },
                    };
                }
            },
            Some(change) = clip::changed(&mut clipboard) => match change {
                clip::Change::Text(text) => {
                    let _ = send_msg(&mut send, &Ask::Clip(text)).await;
                }
                // Files go over only when pasted on their screen (`Cmd::Paste`).
                clip::Change::Files(_) => {}
            },
            _ = tick.tick() => {
                let (kind, ms) = net::link(&conn);
                out.send(id, ViewEvent::Link { kind, ms, fps: shown.swap(0, Ordering::Relaxed) });
            }
            Some(done) = transfers.join_next() => {
                if let Ok(n) = done {
                    running.remove(&n);
                }
            }
        }
    };
    decoder.abort();
    sound.abort();
    transfers.abort_all();
    outcome
}

fn connect_error(e: &str) -> String {
    let lower = e.to_lowercase();
    if lower.contains("alpn") || lower.contains("application protocol") {
        "That computer runs an older Deskline. Ask them to update it.".into()
    } else {
        format!("Couldn't reach that computer: {e}")
    }
}

/// A picture for the window, written as RGBA bytes by the decoder.
struct Pixels(ColorImage);

impl AsMut<[u8]> for Pixels {
    fn as_mut(&mut self) -> &mut [u8] {
        let px = &mut self.0.pixels;
        // SAFETY: Color32 is repr(C) over [u8; 4] (RGBA, premultiplied; the picture is opaque,
        // so premultiplied and plain are the same bytes), so the pixels are 4 bytes each.
        unsafe { std::slice::from_raw_parts_mut(px.as_mut_ptr().cast::<u8>(), px.len() * 4) }
    }
}

async fn decode(
    mut video: RecvStream,
    acks: mpsc::UnboundedSender<Cmd>,
    slot: Arc<Mutex<Option<ColorImage>>>,
    out: Out,
    shown: Arc<AtomicU32>,
) -> anyhow::Result<()> {
    let mut dec = codec::Decoder::new()?;
    loop {
        let p: Packet = recv_msg(&mut video).await?;
        // Straight into the window's image: at 4K and 120 fps an extra copy is 4 GB/s.
        let pic = tokio::task::block_in_place(|| {
            dec.decode_into(&p.data, |w, h| Pixels(ColorImage::filled([w, h], Color32::BLACK)))
        });
        match pic {
            Ok(Some((_, _, Pixels(img)))) => {
                *slot.lock().unwrap() = Some(img);
                shown.fetch_add(1, Ordering::Relaxed);
                out.repaint();
            }
            Ok(None) => {}
            Err(_) => {
                let _ = acks.send(Cmd::Ask(Ask::Keyframe));
            }
        }
        if acks.send(Cmd::Ask(Ask::Ack(p.seq))).is_err() {
            return Ok(());
        }
    }
}

/// Sound arrives as datagrams; the player starts with the first one.
async fn play(conn: Connection, out: Out, id: u64) {
    let mut player = None;
    while let Ok(d) = conn.read_datagram().await {
        if player.is_none() {
            match audio::Player::new() {
                Ok(p) => player = Some(p),
                Err(e) => {
                    out.send(id, ViewEvent::Note(format!("Can't play their sound: {e}")));
                    return;
                }
            }
        }
        if let Some(p) = player.as_mut() {
            p.push(&d);
        }
    }
}

/// One transfer's events for the window: progress at most ~7 times a second, then how it ended.
struct Report {
    id: u64,
    out: Out,
    session: u64,
    /// The transfer so far, and when the window last heard of it.
    now: Mutex<(Transfer, Option<Instant>)>,
}

impl Report {
    fn new(out: &Out, session: u64, id: u64, what: What) -> Self {
        let t = Transfer { id, what, done: 0, total: 0, end: None };
        Self { id, out: out.clone(), session, now: Mutex::new((t, None)) }
    }

    fn progress(&self, done: u64, total: u64) {
        let mut now = self.now.lock().unwrap();
        let (t, last) = &mut *now;
        (t.done, t.total) = (done, total);
        if last.is_none_or(|l| l.elapsed() >= Duration::from_millis(150)) {
            *last = Some(Instant::now());
            self.out.send(self.session, ViewEvent::Transfer(t.clone()));
        }
    }

    /// It's over; returns its number, what the transfer tasks return.
    fn end(&self, how: anyhow::Result<String>) -> u64 {
        let mut t = self.now.lock().unwrap().0.clone();
        t.done = t.total;
        t.end = Some(how.map_err(|e| format!("{e:#}")));
        self.out.send(self.session, ViewEvent::Transfer(t));
        self.id
    }
}

/// The last part of a path from either system.
fn file_name(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    s.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(&s).to_string()
}

async fn upload(conn: Connection, path: PathBuf, to: Option<Dest>, r: Report) -> u64 {
    r.end(files::upload(&conn, &path, to, &|done, total| r.progress(done, total)).await)
}

async fn download(conn: Connection, remote: String, r: Report) -> u64 {
    let saved = async {
        let dir = files::downloads()?;
        files::download(&conn, &remote, &dir, &|done, total| r.progress(done, total)).await
    };
    r.end(saved.await.map(|p| p.display().to_string()))
}

/// A new number for each paste: each lands in a fresh folder of its own.
fn paste_number() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

/// Ctrl+V on their screen with files on this computer's clipboard: they go into a fresh folder
/// over there and onto that clipboard, then the paste goes through.
async fn paste_there(conn: Connection, paths: Vec<PathBuf>, acks: mpsc::UnboundedSender<Cmd>, r: Report) -> u64 {
    let sent = async {
        let n = paste_number();
        let total = files::size(paths.clone()).await;
        let (base, last) = (AtomicU64::new(0), AtomicU64::new(0));
        let mut landed = Vec::new();
        for p in &paths {
            let progress = |done, of| {
                last.store(of, Ordering::Relaxed);
                r.progress(base.load(Ordering::Relaxed) + done, total);
            };
            landed.push(files::upload(&conn, p, Some(Dest::Clipboard(n)), &progress).await?);
            base.fetch_add(last.swap(0, Ordering::Relaxed), Ordering::Relaxed);
        }
        let _ = acks.send(Cmd::Ask(Ask::ClipFiles(landed)));
        // By now the Ctrl that made the paste may be up: press it again around V.
        for (code, down) in [(CTRL, true), (V, true), (V, false), (CTRL, false)] {
            let _ = acks.send(Cmd::Ask(Ask::Input(Input::Key { code, down })));
        }
        anyhow::Ok(String::new())
    };
    r.end(sent.await)
}

/// Files copied on their computer: into a fresh folder here, then onto this computer's
/// clipboard, ready to paste.
async fn copy_here(conn: Connection, paths: Vec<String>, total: u64, acks: mpsc::UnboundedSender<Cmd>, r: Report) -> u64 {
    let got = async {
        let n = paste_number();
        let dir = tokio::task::spawn_blocking(move || files::clipboard_dir(n)).await??;
        let (base, last) = (AtomicU64::new(0), AtomicU64::new(0));
        let mut here = Vec::new();
        for p in &paths {
            let progress = |done, of| {
                last.store(of, Ordering::Relaxed);
                r.progress(base.load(Ordering::Relaxed) + done, total);
            };
            here.push(files::download(&conn, p, &dir, &progress).await?);
            base.fetch_add(last.swap(0, Ordering::Relaxed), Ordering::Relaxed);
        }
        let _ = acks.send(Cmd::Clipboard(here));
        anyhow::Ok(dir.display().to_string())
    };
    r.end(got.await)
}
