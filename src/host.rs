//! Sharing this screen. A viewer gets in when someone clicks Accept, with the unattended-access
//! password, or because it's trusted; then the screen streams to it and, with control, its
//! mouse, keyboard, clipboard and files reach this computer. Several viewers can watch at once;
//! they share one capture.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, ConnectionError, RecvStream, SendStream};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot, watch};
use tokio::time::{Instant, timeout};

use crate::codec::{self, Quality};
use crate::desktop::{Frame, Screen};
use crate::proto::{self, Ask, Hello, Packet, STOPPED, Tell, recv_msg, send_msg};
use crate::settings::{self, Peer, Store};
use crate::{audio, clip, files, net};

const FRAME_GAP: Duration = Duration::from_millis(33);
/// A viewer whose link dropped gets straight back in for this long.
const GRACE: Duration = Duration::from_secs(90);
const MAX_SESSIONS: usize = 8;

/// The host side to the window.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum HostEvent {
    /// Forget what you knew; the current state follows.
    Reset,
    /// This computer's code.
    Code(String),
    Security { password: bool, trusted: Vec<Peer> },
    /// Accept or decline `who`: answer with `HostCmd::Answer`.
    Request { req: u64, who: String },
    /// Answered elsewhere, timed out, or the viewer gave up.
    RequestGone { req: u64 },
    Sharing { id: u64, who: String, control: bool },
    ControlRequest { id: u64 },
    Control { id: u64, on: bool },
    Chat { id: u64, text: String },
    Note { id: u64, text: String },
    ShareEnded { id: u64, why: String },
    Error(String),
}

/// The window to the host side.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum HostCmd {
    /// Send the current state (a window just connected).
    Hello,
    Answer { req: u64, yes: bool, control: bool, trust: bool },
    AllowControl { id: u64, yes: bool },
    Stop { id: u64 },
    Chat { id: u64, text: String },
    /// The unattended-access password; None turns it off.
    SetPassword(Option<String>),
    Untrust(String),
}

impl HostCmd {
    /// Changes who gets in without a click: from another process, only an elevated one may.
    pub fn is_admin(&self) -> bool {
        matches!(self, HostCmd::SetPassword(_) | HostCmd::Untrust(_))
    }
}

/// How the host side reaches the window: channels in the same process, or the service's pipe.
pub struct UiLink {
    pub events: mpsc::UnboundedSender<HostEvent>,
    pub cmds: mpsc::UnboundedReceiver<HostCmd>,
    /// A window is there to answer prompts.
    pub present: watch::Receiver<bool>,
    /// Open a window when there's none (the service, when someone asks to connect).
    pub summon: Option<fn() -> anyhow::Result<()>>,
}

struct Host {
    ep: Endpoint,
    dir: PathBuf,
    store: Store,
    name: String,
    events: mpsc::UnboundedSender<HostEvent>,
    present: watch::Receiver<bool>,
    summon: Option<fn() -> anyhow::Result<()>>,
    capture: AsyncMutex<Option<Capture>>,
    sessions: Mutex<HashMap<u64, Live>>,
    prompts: Mutex<HashMap<u64, (String, oneshot::Sender<Answer>)>>,
    /// Viewers whose link dropped, and whether they had control.
    recent: Mutex<HashMap<EndpointId, (Instant, bool)>>,
    failures: Mutex<Failures>,
}

/// The one capture all sessions share.
struct Capture {
    screen: Arc<AsyncMutex<Screen>>,
    frames: watch::Receiver<Option<Arc<Frame>>>,
    users: usize,
}

struct Live {
    peer: EndpointId,
    who: String,
    control: bool,
    tx: mpsc::UnboundedSender<Cmd>,
}

enum Cmd {
    Control(bool),
    Chat(String),
    Stop,
    /// The same viewer came back on a new connection; this one is dead.
    Replaced,
}

struct Answer {
    yes: bool,
    control: bool,
    trust: bool,
}

static IDS: AtomicU64 = AtomicU64::new(1);

pub fn start(ep: Endpoint, dir: PathBuf, store: Store, name: String, ui: UiLink) {
    let UiLink { events, cmds, present, summon } = ui;
    let host = Arc::new(Host {
        ep,
        dir,
        store,
        name,
        events,
        present,
        summon,
        capture: AsyncMutex::new(None),
        sessions: Mutex::default(),
        prompts: Mutex::default(),
        recent: Mutex::default(),
        failures: Mutex::default(),
    });
    host.resync();
    tokio::spawn(host.clone().commands(cmds));
    tokio::spawn(async move {
        while let Some(inc) = host.ep.accept().await {
            let host = host.clone();
            tokio::spawn(async move {
                if let Ok(conn) = inc.await {
                    let _ = host.serve(conn).await;
                }
            });
        }
    });
}

impl Host {
    fn send(&self, e: HostEvent) {
        let _ = self.events.send(e);
    }

    fn security(&self) {
        let s = self.store.get();
        self.send(HostEvent::Security { password: s.password.is_some(), trusted: s.trusted });
    }

    fn resync(&self) {
        self.send(HostEvent::Reset);
        self.send(HostEvent::Code(net::code(self.ep.id())));
        self.security();
        for (&id, l) in self.sessions.lock().unwrap().iter() {
            self.send(HostEvent::Sharing { id, who: l.who.clone(), control: l.control });
        }
        for (&req, (who, _)) in self.prompts.lock().unwrap().iter() {
            self.send(HostEvent::Request { req, who: who.clone() });
        }
    }

    fn to(&self, id: u64, cmd: Cmd) {
        if let Some(l) = self.sessions.lock().unwrap().get(&id) {
            let _ = l.tx.send(cmd);
        }
    }

    async fn commands(self: Arc<Self>, mut cmds: mpsc::UnboundedReceiver<HostCmd>) {
        while let Some(c) = cmds.recv().await {
            match c {
                HostCmd::Hello => self.resync(),
                HostCmd::Answer { req, yes, control, trust } => {
                    if let Some((_, tx)) = self.prompts.lock().unwrap().remove(&req) {
                        let _ = tx.send(Answer { yes, control, trust });
                    }
                }
                HostCmd::AllowControl { id, yes } => self.to(id, Cmd::Control(yes)),
                HostCmd::Stop { id } => self.to(id, Cmd::Stop),
                HostCmd::Chat { id, text } => self.to(id, Cmd::Chat(text)),
                HostCmd::SetPassword(p) => {
                    let store = self.store.clone();
                    match tokio::task::spawn_blocking(move || store.set_password(p.as_deref())).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => self.send(HostEvent::Error(e.to_string())),
                        Err(e) => self.send(HostEvent::Error(e.to_string())),
                    }
                    self.security();
                }
                HostCmd::Untrust(id) => {
                    if let Err(e) = self.store.update(|s| s.trusted.retain(|p| p.id != id)) {
                        self.send(HostEvent::Error(e.to_string()));
                    }
                    self.security();
                }
            }
        }
    }

    async fn serve(self: Arc<Self>, conn: Connection) -> anyhow::Result<()> {
        let (mut send, mut recv) = conn.accept_bi().await?;
        let hello: Hello = recv_msg(&mut recv).await?;
        let peer = conn.remote_id();
        let who = format!("{} ({})", proto::clean(&hello.name), net::short(peer));

        let refuse = async |send: &mut SendStream, why: String, password: bool| -> anyhow::Result<()> {
            settings::log(&self.dir, &format!("{who}  refused: {why}"));
            send_msg(send, &Tell::Denied { why, password }).await?;
            send.finish()?;
            let _ = timeout(Duration::from_secs(3), conn.closed()).await;
            Ok(())
        };
        let (mut control, how) = match self.admit(&conn, peer, &who, &hello).await {
            Ok(ok) => ok,
            Err((why, password)) => return refuse(&mut send, why, password).await,
        };
        let (screen, frames) = match self.attach().await {
            Ok(s) => s,
            Err(e) => {
                self.send(HostEvent::Error(format!("Couldn't share your screen: {e}")));
                return refuse(&mut send, format!("Their screen couldn't be shared: {e}"), false).await;
            }
        };

        let id = IDS.fetch_add(1, Ordering::Relaxed);
        let (tx, cmds) = mpsc::unbounded_channel();
        self.sessions.lock().unwrap().insert(id, Live { peer, who: who.clone(), control, tx });
        self.send(HostEvent::Sharing { id, who: who.clone(), control });
        settings::log(&self.dir, &format!("{who}  connected ({how}){}", if control { " with control" } else { "" }));
        let started = Instant::now();

        let mut privacy = false;
        let end = self.share(id, &who, &conn, &mut send, recv, &screen, frames, cmds, &mut control, &mut privacy).await;
        let end = end.unwrap_or_else(|e| End::Lost(format!("Connection lost: {e}")));

        self.sessions.lock().unwrap().remove(&id);
        if privacy {
            let _ = screen.lock().await.privacy(false).await;
        }
        self.detach(screen).await;
        let why = match end {
            End::Lost(why) => {
                self.recent.lock().unwrap().insert(peer, (Instant::now(), control));
                why
            }
            End::Stopped(why) => {
                conn.close(STOPPED.into(), b"stopped");
                why
            }
            End::Bye(why) => {
                conn.close(0u8.into(), b"bye");
                why
            }
        };
        let mins = started.elapsed().as_secs().div_ceil(60);
        settings::log(&self.dir, &format!("{who}  ended after {mins} min: {why}"));
        self.send(HostEvent::ShareEnded { id, why });
        Ok(())
    }

    /// Whether `peer` gets in, with control or not, and how; else why not and whether the
    /// password would help.
    async fn admit(&self, conn: &Connection, peer: EndpointId, who: &str, hello: &Hello) -> Result<(bool, &'static str), (String, bool)> {
        if self.sessions.lock().unwrap().len() >= MAX_SESSIONS {
            return Err(("That computer has too many sessions open.".into(), false));
        }
        let id = peer.to_z32();
        let name = proto::clean(&hello.name);
        if self.store.trusts(&id) {
            return Ok((true, "trusted"));
        }
        if hello.resume {
            if let Some(control) = self.take_back(peer) {
                return Ok((control, "reconnected"));
            }
        }
        let hash = self.store.get().password;
        let has_password = hash.is_some();
        if let (Some(pw), Some(hash)) = (hello.password.clone(), hash) {
            if self.failures.lock().unwrap().locked(peer) {
                return Err(("Too many wrong passwords. Try again in a few minutes.".into(), true));
            }
            let ok = tokio::task::spawn_blocking(move || settings::verify(&pw, &hash)).await.unwrap_or(false);
            if !ok {
                self.failures.lock().unwrap().failed(peer);
                return Err(("Wrong password.".into(), true));
            }
            self.failures.lock().unwrap().passed(peer);
            if hello.remember {
                let _ = self.store.trust(&id, &name);
                self.security();
            }
            return Ok((true, "password"));
        }

        if !self.reach_window().await {
            let why = if has_password {
                "Nobody is at that computer to accept. Use its unattended-access password."
            } else {
                "Nobody is at that computer to accept, and it has no unattended-access password."
            };
            return Err((why.into(), has_password));
        }
        let req = IDS.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.prompts.lock().unwrap().insert(req, (who.to_string(), tx));
        self.send(HostEvent::Request { req, who: who.to_string() });
        let answer = tokio::select! {
            a = timeout(Duration::from_secs(60), rx) => a.ok().and_then(Result::ok),
            _ = conn.closed() => None,
        };
        if self.prompts.lock().unwrap().remove(&req).is_some() {
            self.send(HostEvent::RequestGone { req });
        }
        match answer {
            Some(a) if a.yes => {
                if a.trust {
                    let _ = self.store.trust(&id, &name);
                    self.security();
                }
                Ok((a.control, "accepted"))
            }
            Some(_) => Err(("They declined.".into(), has_password)),
            None => Err(("Nobody answered.".into(), has_password)),
        }
    }

    /// A reconnecting viewer: from the list of dropped links, or still listed as live because
    /// this side hasn't noticed the old connection died.
    fn take_back(&self, peer: EndpointId) -> Option<bool> {
        if let Some((at, control)) = self.recent.lock().unwrap().remove(&peer) {
            if at.elapsed() < GRACE {
                return Some(control);
            }
        }
        let sessions = self.sessions.lock().unwrap();
        let old = sessions.values().find(|l| l.peer == peer)?;
        let _ = old.tx.send(Cmd::Replaced);
        Some(old.control)
    }

    async fn reach_window(&self) -> bool {
        let mut present = self.present.clone();
        if *present.borrow() {
            return true;
        }
        let Some(summon) = self.summon else { return false };
        if summon().is_err() {
            return false;
        }
        matches!(timeout(Duration::from_secs(15), present.wait_for(|p| *p)).await, Ok(Ok(_)))
    }

    async fn attach(&self) -> anyhow::Result<(Arc<AsyncMutex<Screen>>, watch::Receiver<Option<Arc<Frame>>>)> {
        let mut slot = self.capture.lock().await;
        if slot.is_none() {
            let screen = Screen::open(&self.dir).await?;
            *slot = Some(Capture { frames: screen.frames.clone(), screen: Arc::new(AsyncMutex::new(screen)), users: 0 });
        }
        let c = slot.as_mut().expect("just set");
        c.users += 1;
        Ok((c.screen.clone(), c.frames.clone()))
    }

    async fn detach(&self, screen: Arc<AsyncMutex<Screen>>) {
        drop(screen);
        let mut slot = self.capture.lock().await;
        let Some(c) = slot.as_mut() else { return };
        c.users -= 1;
        if c.users == 0 {
            let c = slot.take().expect("checked");
            if let Ok(screen) = Arc::try_unwrap(c.screen) {
                screen.into_inner().close().await;
            }
        }
    }

    fn set_control(&self, id: u64, on: bool) {
        if let Some(l) = self.sessions.lock().unwrap().get_mut(&id) {
            l.control = on;
        }
        self.send(HostEvent::Control { id, on });
    }

    #[allow(clippy::too_many_arguments)]
    async fn share(
        self: &Arc<Self>,
        id: u64,
        who: &str,
        conn: &Connection,
        send: &mut SendStream,
        recv: RecvStream,
        screen: &Arc<AsyncMutex<Screen>>,
        frames: watch::Receiver<Option<Arc<Frame>>>,
        mut cmds: mpsc::UnboundedReceiver<Cmd>,
        control: &mut bool,
        privacy: &mut bool,
    ) -> anyhow::Result<End> {
        send_msg(send, &Tell::Accepted { name: self.name.clone(), control: *control }).await?;
        let (list, current) = {
            let s = screen.lock().await;
            (s.monitors(), s.current())
        };
        send_msg(send, &Tell::Monitors { list, current }).await?;

        let video = conn.open_uni().await?;
        let (acked_tx, acked) = watch::channel(0u32);
        let (quality_tx, quality) = watch::channel(Quality::default());
        let key = Arc::new(Notify::new());
        let mut pump = tokio::spawn(pump(conn.clone(), frames, video, acked, key.clone(), quality));
        let mut asks = proto::reader::<Ask>(recv);
        let mut clipboard = if *control { clip::Clipboard::watch().ok() } else { None };
        let mut sound: Option<(audio::Capture, mpsc::Receiver<Vec<u8>>)> = None;
        let mut asked = false;

        let end = loop {
            tokio::select! {
                a = asks.recv() => match a {
                    Some(Ask::Input(i)) if *control => {
                        let _ = screen.lock().await.input(i).await;
                    }
                    Some(Ask::Ack(n)) => {
                        acked_tx.send_replace(n);
                    }
                    Some(Ask::Keyframe) => key.notify_one(),
                    Some(Ask::WantControl) => {
                        if *control {
                            send_msg(send, &Tell::Control(true)).await?;
                        } else if !asked {
                            asked = true;
                            self.send(HostEvent::ControlRequest { id });
                        }
                    }
                    Some(Ask::Monitor(n)) => {
                        let mut s = screen.lock().await;
                        if let Err(e) = s.select(n).await {
                            send_msg(send, &Tell::Note(format!("Couldn't switch screens: {e}"))).await?;
                        }
                        let (list, current) = (s.monitors(), s.current());
                        drop(s);
                        key.notify_one();
                        send_msg(send, &Tell::Monitors { list, current }).await?;
                    }
                    Some(Ask::Clip(text)) if *control => {
                        if let Some(c) = &clipboard {
                            c.set(text);
                        }
                    }
                    Some(Ask::Chat(text)) => self.send(HostEvent::Chat { id, text: proto::clean_chat(&text) }),
                    Some(Ask::Sound(on)) => {
                        sound = None;
                        if on {
                            // Opening the device blocks (up to 3 s on Linux).
                            match tokio::task::block_in_place(audio::Capture::start) {
                                Ok(s) => sound = Some(s),
                                Err(e) => send_msg(send, &Tell::Note(format!("No sound from this computer: {e}"))).await?,
                            }
                        }
                    }
                    Some(Ask::Privacy(on)) if *control => match screen.lock().await.privacy(on).await {
                        Ok(()) => {
                            *privacy = on;
                            send_msg(send, &Tell::Privacy(on)).await?;
                        }
                        Err(e) => {
                            send_msg(send, &Tell::Note(e.to_string())).await?;
                            send_msg(send, &Tell::Privacy(*privacy)).await?;
                        }
                    },
                    Some(Ask::Quality(q)) => {
                        quality_tx.send_replace(q);
                    }
                    Some(Ask::Bye) => break End::Bye("They disconnected.".into()),
                    Some(_) => {}
                    None => break match conn.close_reason() {
                        Some(ConnectionError::ApplicationClosed(_)) | None => End::Bye("They disconnected.".into()),
                        Some(_) => End::Lost("Connection lost.".into()),
                    },
                },
                Some(text) = clip::changed(&mut clipboard) => send_msg(send, &Tell::Clip(text)).await?,
                Some(packet) = next_sound(&mut sound) => {
                    // Too big or the link is full: that bit of sound is gone, which is fine.
                    let _ = conn.send_datagram(packet.into());
                }
                c = cmds.recv() => match c {
                    Some(Cmd::Control(on)) => {
                        asked = false;
                        if *control != on {
                            *control = on;
                            if on {
                                clipboard = clip::Clipboard::watch().ok();
                            } else {
                                clipboard = None;
                                let mut s = screen.lock().await;
                                s.release().await;
                                if *privacy && s.privacy(false).await.is_ok() {
                                    *privacy = false;
                                    send_msg(send, &Tell::Privacy(false)).await?;
                                }
                            }
                        }
                        self.set_control(id, on);
                        send_msg(send, &Tell::Control(on)).await?;
                    }
                    Some(Cmd::Chat(text)) => send_msg(send, &Tell::Chat(text)).await?,
                    Some(Cmd::Replaced) => break End::Lost("Reconnected.".into()),
                    Some(Cmd::Stop) | None => break End::Stopped("You stopped sharing.".into()),
                },
                s = conn.accept_bi() => match s {
                    Ok((fs, fr)) if *control => {
                        let (host, who) = (self.clone(), who.to_string());
                        tokio::spawn(async move {
                            let line = match files::serve(fs, fr).await {
                                Ok(Some(line)) => line,
                                Ok(None) => return,
                                Err(e) => format!("File transfer failed: {e}"),
                            };
                            settings::log(&host.dir, &format!("{who}  {line}"));
                            host.send(HostEvent::Note { id, text: line });
                        });
                    }
                    // Files need control.
                    Ok((mut fs, _)) => {
                        let _ = fs.reset(0u32.into());
                    }
                    Err(_) => {}
                },
                r = &mut pump => break End::Stopped(match r {
                    Ok(Err(e)) => format!("Sharing stopped: {e}"),
                    _ => "Sharing stopped.".into(),
                }),
            }
        };
        pump.abort();
        Ok(end)
    }
}

enum End {
    /// Ended on purpose by the viewer.
    Bye(String),
    /// Ended here; the viewer shouldn't try to come back.
    Stopped(String),
    /// The link died; the viewer may come back within `GRACE`.
    Lost(String),
}

async fn next_sound(s: &mut Option<(audio::Capture, mpsc::Receiver<Vec<u8>>)>) -> Option<Vec<u8>> {
    match s {
        Some((_, rx)) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Wrong-password brakes: 5 misses lock that viewer out for 5 minutes; 30 misses from anyone
/// in 10 minutes lock everyone out until they age out.
#[derive(Default)]
struct Failures {
    peers: HashMap<EndpointId, (u32, Instant)>,
    all: Vec<Instant>,
}

impl Failures {
    fn locked(&mut self, peer: EndpointId) -> bool {
        self.all.retain(|t| t.elapsed() < Duration::from_secs(600));
        self.all.len() >= 30 || self.peers.get(&peer).is_some_and(|(n, at)| *n >= 5 && at.elapsed() < Duration::from_secs(300))
    }

    fn failed(&mut self, peer: EndpointId) {
        self.all.push(Instant::now());
        let e = self.peers.entry(peer).or_insert((0, Instant::now()));
        if e.1.elapsed() >= Duration::from_secs(300) {
            e.0 = 0;
        }
        e.0 += 1;
        e.1 = Instant::now();
    }

    fn passed(&mut self, peer: EndpointId) {
        self.peers.remove(&peer);
    }
}

/// Capture -> encode -> send, newest frame only, at most 30 fps. Enough frames may be unacked
/// to fill one round trip (a long link isn't capped at 2 per RTT); when acks hold the pump back
/// for a good part of each second the link is full, so the encoder is asked for fewer bits,
/// and for more again once it's been calm for a while.
async fn pump(
    conn: Connection,
    mut frames: watch::Receiver<Option<Arc<Frame>>>,
    mut out: SendStream,
    mut acked: watch::Receiver<u32>,
    key: Arc<Notify>,
    mut quality: watch::Receiver<Quality>,
) -> anyhow::Result<()> {
    let mut enc = codec::Encoder::new(*quality.borrow_and_update());
    let mut seq = 0u32;
    let mut last: Option<Arc<Frame>> = None;
    let mut rate = 1.0f32;
    let (mut second, mut blocked, mut calm) = (Instant::now(), Duration::ZERO, 0);
    loop {
        let in_flight = (net::rtt(&conn).as_millis() / FRAME_GAP.as_millis() + 2).clamp(2, 8) as u32;
        let waited = Instant::now();
        while seq.wrapping_sub(*acked.borrow_and_update()) >= in_flight {
            acked.changed().await?;
        }
        blocked += waited.elapsed();
        if second.elapsed() >= Duration::from_secs(1) {
            if blocked > Duration::from_millis(300) {
                rate = (rate * 0.7).max(0.1);
                calm = 0;
                enc.set_rate(rate);
            } else if blocked < Duration::from_millis(50) && rate < 1.0 {
                calm += 1;
                if calm >= 3 {
                    rate = (rate * 1.15).min(1.0);
                    enc.set_rate(rate);
                }
            }
            (second, blocked) = (Instant::now(), Duration::ZERO);
        }
        if quality.has_changed().unwrap_or(false) {
            enc.set_quality(*quality.borrow_and_update());
        }
        // A keyframe request re-encodes the last frame: the screen may be sitting still.
        let force = tokio::select! {
            r = frames.changed() => { r.map_err(|_| anyhow::anyhow!("capture ended"))?; false }
            _ = key.notified(), if last.is_some() => true,
        };
        if !force {
            last = frames.borrow_and_update().clone();
        }
        let Some(frame) = last.clone() else { continue };
        let started = Instant::now();
        let data = tokio::task::block_in_place(|| enc.encode(&frame, force))?;
        if data.is_empty() {
            continue;
        }
        seq = seq.wrapping_add(1);
        send_msg(&mut out, &Packet { seq, data }).await?;
        tokio::time::sleep_until(started + FRAME_GAP).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Input;
    use crate::desktop::imp::INJECTED;
    use crate::viewer::{self, Options, ViewEvent};

    async fn next_host(rx: &mut mpsc::UnboundedReceiver<HostEvent>, what: &str, want: impl Fn(&HostEvent) -> bool) -> HostEvent {
        timeout(Duration::from_secs(30), async {
            loop {
                let e = rx.recv().await.expect("events closed");
                if want(&e) {
                    return e;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never got {what}"))
    }

    async fn next_view(rx: &mut mpsc::UnboundedReceiver<(u64, ViewEvent)>, what: &str, want: impl Fn(&ViewEvent) -> bool) -> ViewEvent {
        timeout(Duration::from_secs(30), async {
            loop {
                let (_, e) = rx.recv().await.expect("events closed");
                if want(&e) {
                    return e;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never got {what}"))
    }

    fn opts(password: Option<&str>, remember: bool) -> Options {
        Options { name: "guest".into(), password: password.map(Into::into), remember, sound: false, quality: Quality::default() }
    }

    /// Real endpoints on this machine, fake screen. Run with
    /// `cargo test two_peers -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn two_peers_decline_accept_control_password_trust_stop() {
        let dir = std::env::temp_dir().join(format!("deskline-test-{}", std::process::id()));
        let (home_a, home_b) = (dir.join("a"), dir.join("b"));
        let (a, b) = (net::start(&home_a, None).await.unwrap(), net::start(&home_b, None).await.unwrap());
        assert_eq!(net::start(&home_a, None).await.unwrap().id(), a.id(), "identity must survive a restart");
        assert_eq!(net::parse_code(&net::code(a.id())), Some(a.id()));

        let (events, mut host) = mpsc::unbounded_channel();
        let (cmd_tx, cmds) = mpsc::unbounded_channel();
        let (_present_tx, present) = watch::channel(true);
        let store = Store::open(&home_a);
        start(a.clone(), home_a.clone(), store.clone(), "host".into(), UiLink { events, cmds, present, summon: None });
        let (view_tx, mut viewer) = mpsc::unbounded_channel();
        let out = viewer::Out::new(view_tx, None);

        // Declined first.
        let v = viewer::view(b.clone(), a.id(), opts(None, false), out.clone());
        let HostEvent::Request { who, req } = next_host(&mut host, "request", |e| matches!(e, HostEvent::Request { .. })).await else { unreachable!() };
        assert!(who.starts_with("guest ("), "{who}");
        cmd_tx.send(HostCmd::Answer { req, yes: false, control: false, trust: false }).unwrap();
        let ViewEvent::Ended { why, password } = next_view(&mut viewer, "decline", |e| matches!(e, ViewEvent::Ended { .. })).await else { unreachable!() };
        assert_eq!((why.as_str(), password), ("They declined.", false));
        drop(v);

        // Accepted without control: frames arrive and decode; input is ignored.
        let v = viewer::view(b.clone(), a.id(), opts(None, false), out.clone());
        let HostEvent::Request { req, .. } = next_host(&mut host, "request", |e| matches!(e, HostEvent::Request { .. })).await else { unreachable!() };
        cmd_tx.send(HostCmd::Answer { req, yes: true, control: false, trust: false }).unwrap();
        let HostEvent::Sharing { id, .. } = next_host(&mut host, "sharing", |e| matches!(e, HostEvent::Sharing { .. })).await else { unreachable!() };
        next_view(&mut viewer, "viewing", |e| matches!(e, ViewEvent::Viewing { who, control: false } if who == "host")).await;
        let ViewEvent::Monitors { list, current } = next_view(&mut viewer, "monitors", |e| matches!(e, ViewEvent::Monitors { .. })).await else { unreachable!() };
        assert_eq!((list.len(), current), (2, 0));
        let mut pictures = 0;
        timeout(Duration::from_secs(15), async {
            while pictures < 10 {
                if let Some(img) = v.frame.lock().unwrap().take() {
                    assert_eq!(img.size, [640, 360]);
                    pictures += 1;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("frames stopped flowing");
        INJECTED.lock().unwrap().clear();
        v.send(Ask::Input(Input::Move { x: 0.1, y: 0.1 }));

        // Control asked and given; then the other monitor.
        v.send(Ask::WantControl);
        next_host(&mut host, "control request", |e| matches!(e, HostEvent::ControlRequest { .. })).await;
        cmd_tx.send(HostCmd::AllowControl { id, yes: true }).unwrap();
        next_view(&mut viewer, "control on", |e| matches!(e, ViewEvent::Control(true))).await;
        v.send(Ask::Input(Input::Move { x: 0.5, y: 0.25 }));
        v.send(Ask::Input(Input::Key { code: 30, down: true }));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(*INJECTED.lock().unwrap(), vec![Input::Move { x: 0.5, y: 0.25 }, Input::Key { code: 30, down: true }]);
        v.send(Ask::Monitor(1));
        next_view(&mut viewer, "monitor 2", |e| matches!(e, ViewEvent::Monitors { current: 1, .. })).await;

        // Chat both ways.
        v.send(Ask::Chat("hi".into()));
        next_host(&mut host, "chat", |e| matches!(e, HostEvent::Chat { text, .. } if text == "hi")).await;
        cmd_tx.send(HostCmd::Chat { id, text: "hello".into() }).unwrap();
        next_view(&mut viewer, "chat back", |e| matches!(e, ViewEvent::Chat(t) if t == "hello")).await;

        // Taking control back releases the held key; stopping ends it for good.
        cmd_tx.send(HostCmd::AllowControl { id, yes: false }).unwrap();
        next_view(&mut viewer, "control off", |e| matches!(e, ViewEvent::Control(false))).await;
        assert_eq!(INJECTED.lock().unwrap().last(), Some(&Input::Key { code: 30, down: false }));
        cmd_tx.send(HostCmd::Stop { id }).unwrap();
        let ViewEvent::Ended { why, .. } = next_view(&mut viewer, "end", |e| matches!(e, ViewEvent::Ended { .. })).await else { unreachable!() };
        assert_eq!(why, "They stopped sharing.");
        drop(v);

        // Unattended: the wrong password is refused, the right one gets in with control and,
        // remembered, the next session needs nothing at all.
        cmd_tx.send(HostCmd::SetPassword(Some("open sesame".into()))).unwrap();
        next_host(&mut host, "password set", |e| matches!(e, HostEvent::Security { password: true, .. })).await;
        let v = viewer::view(b.clone(), a.id(), opts(Some("wrong one!"), false), out.clone());
        let ViewEvent::Ended { why, password } = next_view(&mut viewer, "refused", |e| matches!(e, ViewEvent::Ended { .. })).await else { unreachable!() };
        assert_eq!((why.as_str(), password), ("Wrong password.", true));
        drop(v);
        let v = viewer::view(b.clone(), a.id(), opts(Some("open sesame"), true), out.clone());
        next_view(&mut viewer, "in by password", |e| matches!(e, ViewEvent::Viewing { control: true, .. })).await;
        assert!(store.trusts(&b.id().to_z32()));
        drop(v);
        next_host(&mut host, "ended", |e| matches!(e, HostEvent::ShareEnded { .. })).await;
        let v = viewer::view(b.clone(), a.id(), opts(None, false), out);
        next_view(&mut viewer, "in by trust", |e| matches!(e, ViewEvent::Viewing { control: true, .. })).await;
        drop(v);
        let log = std::fs::read_to_string(home_a.join("sessions.log")).unwrap();
        assert!(log.contains("refused: Wrong password.") && log.contains("connected (trusted) with control"), "{log}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
