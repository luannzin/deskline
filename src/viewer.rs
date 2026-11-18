//! Watching someone else's screen: connect, get in (their click, their password or their
//! trust), show the picture, send input, and the extras: sound, clipboard, chat, files. A link
//! that drops reconnects on its own for a minute.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage};
use iroh::endpoint::{Connection, ConnectionError, RecvStream, VarInt};
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::codec::{self, Quality};
use crate::desktop::Monitor;
use crate::proto::{self, ALPN, Ask, Hello, Packet, STOPPED, Tell, VERSION, recv_msg, send_msg};
use crate::{audio, clip, files, net};

/// What a viewing session tells the window.
#[derive(Debug)]
pub enum ViewEvent {
    Status(String),
    Viewing { who: String, control: bool },
    Control(bool),
    Monitors { list: Vec<Monitor>, current: usize },
    /// "LAN 3 ms".
    Link(String),
    Chat(String),
    Note(String),
    Privacy(bool),
    Listing(files::Listing),
    Transfer { name: String, done: u64, total: u64 },
    /// `password`: their unattended-access password would get you in.
    Ended { why: String, password: bool },
}

/// The window's side of a session.
pub enum Cmd {
    Ask(Ask),
    /// List a folder on their computer ("" for the starting places).
    List(String),
    Upload(PathBuf),
    /// Fetch a file or folder of theirs into this computer's Downloads.
    Download(String),
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
    let decoder = tokio::spawn(decode(video, acks.clone(), slot.clone(), out.clone()));
    let sound = tokio::spawn(play(conn.clone(), out.clone(), id));
    let mut tells = proto::reader::<Tell>(recv);
    let mut clipboard = if control { clip::Clipboard::watch().ok() } else { None };
    let mut transfers = tokio::task::JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));

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
                    });
                }
                Some(Cmd::Upload(path)) => {
                    transfers.spawn(upload(conn.clone(), path, out.clone(), id));
                }
                Some(Cmd::Download(remote)) => {
                    transfers.spawn(download(conn.clone(), remote, out.clone(), id));
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
            Some(text) = clip::changed(&mut clipboard) => {
                let _ = send_msg(&mut send, &Ask::Clip(text)).await;
            }
            _ = tick.tick() => out.send(id, ViewEvent::Link(net::link(&conn))),
            Some(_) = transfers.join_next() => {}
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

async fn decode(
    mut video: RecvStream,
    acks: mpsc::UnboundedSender<Cmd>,
    slot: Arc<Mutex<Option<ColorImage>>>,
    out: Out,
) -> anyhow::Result<()> {
    let mut dec = codec::Decoder::new()?;
    loop {
        let p: Packet = recv_msg(&mut video).await?;
        let pic = tokio::task::block_in_place(|| {
            dec.decode(&p.data).map(|pic| pic.map(|(w, h, rgba)| ColorImage::from_rgba_unmultiplied([w, h], &rgba)))
        });
        match pic {
            Ok(Some(img)) => {
                *slot.lock().unwrap() = Some(img);
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

/// Progress for one transfer, at most ~7 events a second plus the last one.
fn progress(out: Out, id: u64, name: String) -> impl Fn(u64, u64) + Send + Sync {
    let last: Mutex<Option<Instant>> = Mutex::new(None);
    move |done, total| {
        let mut last = last.lock().unwrap();
        if done >= total || last.is_none_or(|t| t.elapsed() >= Duration::from_millis(150)) {
            *last = Some(Instant::now());
            out.send(id, ViewEvent::Transfer { name: name.clone(), done, total });
        }
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

async fn upload(conn: Connection, path: PathBuf, out: Out, id: u64) {
    let name = file_name(&path);
    let report = progress(out.clone(), id, name.clone());
    let text = match files::upload(&conn, &path, &report).await {
        Ok(at) => format!("Sent {name} to {at}"),
        Err(e) => format!("Couldn't send {name}: {e}"),
    };
    out.send(id, ViewEvent::Note(text));
}

async fn download(conn: Connection, remote: String, out: Out, id: u64) {
    let name = file_name(std::path::Path::new(&remote));
    let report = progress(out.clone(), id, name.clone());
    let saved = match files::downloads() {
        Ok(dir) => files::download(&conn, &remote, &dir, &report).await,
        Err(e) => Err(e),
    };
    let text = match saved {
        Ok(path) => format!("Saved {}", path.display()),
        Err(e) => format!("Couldn't download {name}: {e}"),
    };
    out.send(id, ViewEvent::Note(text));
}
