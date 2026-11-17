//! The link: iroh QUIC dialed by endpoint id (hole punching, relay fallback, mDNS on the LAN),
//! a persistent identity, and both sides of a session.
//!
//! The viewer opens one bidirectional stream (hello, control requests, input, acks) and the
//! sharer opens one unidirectional stream for video. The viewer acks every frame; the sharer
//! keeps at most two unacked, so a slow link drops frames instead of building up delay.
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, ColorImage};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointId, SecretKey, TransportAddr};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::time::{Instant, timeout};

use crate::codec;
use crate::desktop::{Frame, Input, Screen};

pub const ALPN: &[u8] = b"deskline/1";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const FRAME_GAP: Duration = Duration::from_millis(33);
const IN_FLIGHT: u32 = 2;

/// Viewer to sharer.
#[derive(Serialize, Deserialize, Debug)]
pub enum Ask {
    Hello { version: String, name: String },
    WantControl,
    Input(Input),
    Ack(u32),
    Keyframe,
    Bye,
}

/// Sharer to viewer.
#[derive(Serialize, Deserialize, Debug)]
enum Tell {
    Accepted { name: String },
    Denied(String),
    Control(bool),
}

#[derive(Serialize, Deserialize)]
struct Packet {
    seq: u32,
    data: Vec<u8>,
}

/// What the network tells the window. `id` ties an event to one session so a late event from
/// a closed session can't touch the next one.
pub enum Event {
    /// Someone wants to see this screen.
    Request { who: String, reply: oneshot::Sender<bool> },
    Sharing { id: u64, who: String, ctl: mpsc::UnboundedSender<HostCmd> },
    ControlRequest { id: u64, reply: oneshot::Sender<bool> },
    /// `id` 0: sharing never started.
    ShareEnded { id: u64, why: String },
    Status { id: u64, text: String },
    Viewing { id: u64, who: String },
    Control { id: u64, on: bool },
    Link { id: u64, text: String },
    ViewEnded { id: u64, why: String },
}

pub enum HostCmd {
    Revoke,
    Stop,
}

#[derive(Clone)]
pub struct Events {
    tx: mpsc::UnboundedSender<Event>,
    ctx: egui::Context,
}

impl Events {
    pub fn new(ctx: egui::Context) -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx, ctx }, rx)
    }

    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

static IDS: AtomicU64 = AtomicU64::new(1);

/// Bind the endpoint with this machine's saved identity, so its code never changes.
pub async fn start(home: &Path) -> anyhow::Result<Endpoint> {
    let ep = Endpoint::builder(presets::N0).secret_key(identity(home)?).alpns(vec![ALPN.to_vec()]).bind().await?;
    let mdns = MdnsAddressLookup::builder().build(ep.id())?;
    ep.address_lookup()?.add(mdns);
    Ok(ep)
}

fn identity(home: &Path) -> anyhow::Result<SecretKey> {
    let path = home.join("identity");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(SecretKey::from_bytes(&bytes));
        }
    }
    let key = SecretKey::generate();
    std::fs::create_dir_all(home)?;
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    file.open(&path)?.write_all(&key.to_bytes())?;
    Ok(key)
}

/// The code people share: z-base-32 of the public key, in groups of four.
pub fn code(id: EndpointId) -> String {
    let z = id.to_z32();
    z.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

/// Accepts the code with or without spaces or dashes.
pub fn parse_code(s: &str) -> Option<EndpointId> {
    let z: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    EndpointId::from_z32(&z).ok()
}

pub fn short(id: EndpointId) -> String {
    id.to_z32()[..8].to_string()
}

// ---------------------------------------------------------------- sharing (the host side)

/// Accept incoming sessions forever. One at a time; others get "busy".
pub fn listen(ep: Endpoint, home: std::path::PathBuf, name: String, events: Events) {
    let busy = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        while let Some(inc) = ep.accept().await {
            let (home, name, events, busy) = (home.clone(), name.clone(), events.clone(), busy.clone());
            tokio::spawn(async move {
                if let Ok(conn) = inc.await {
                    let _ = serve(conn, &home, name, events, busy).await;
                }
            });
        }
    });
}

struct Busy(Arc<AtomicBool>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn serve(conn: Connection, home: &Path, name: String, events: Events, busy: Arc<AtomicBool>) -> anyhow::Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let Ask::Hello { version, name: them } = recv_msg(&mut recv).await? else { anyhow::bail!("expected hello") };
    let who = format!("{} ({})", clean(&them), short(conn.remote_id()));

    let refuse = async |send: &mut SendStream, why: String| -> anyhow::Result<()> {
        send_msg(send, &Tell::Denied(why)).await?;
        send.finish()?;
        let _ = timeout(Duration::from_secs(3), conn.closed()).await;
        Ok(())
    };
    if version != VERSION {
        return refuse(&mut send, format!("They run Deskline v{VERSION} and you have v{version}. Both need the same version.")).await;
    }
    if busy.swap(true, Ordering::SeqCst) {
        return refuse(&mut send, "They're already in a session.".into()).await;
    }
    let _busy = Busy(busy);

    let (reply, answer) = oneshot::channel();
    events.send(Event::Request { who: who.clone(), reply });
    let yes = tokio::select! {
        a = timeout(Duration::from_secs(60), answer) => matches!(a, Ok(Ok(true))),
        _ = conn.closed() => return Ok(()),
    };
    if !yes {
        return refuse(&mut send, "They declined.".into()).await;
    }

    let screen = match Screen::open(home).await {
        Ok(s) => s,
        Err(e) => {
            events.send(Event::ShareEnded { id: 0, why: format!("Couldn't share your screen: {e}") });
            return refuse(&mut send, format!("Their screen couldn't be shared: {e}")).await;
        }
    };
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    let (ctl, cmds) = mpsc::unbounded_channel();
    events.send(Event::Sharing { id, who, ctl });

    let mut screen = screen;
    let why = match share(&conn, &mut send, recv, &mut screen, cmds, &events, id, name).await {
        Ok(why) => why,
        Err(e) => format!("Connection lost: {e}"),
    };
    screen.close().await;
    conn.close(0u8.into(), b"bye");
    events.send(Event::ShareEnded { id, why });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn share(
    conn: &Connection,
    send: &mut SendStream,
    recv: RecvStream,
    screen: &mut Screen,
    mut cmds: mpsc::UnboundedReceiver<HostCmd>,
    events: &Events,
    id: u64,
    name: String,
) -> anyhow::Result<String> {
    send_msg(send, &Tell::Accepted { name }).await?;
    let video = conn.open_uni().await?;
    let (acked_tx, acked) = watch::channel(0u32);
    let key = Arc::new(Notify::new());
    let mut pump = tokio::spawn(pump(screen.frames.clone(), video, acked, key.clone()));
    let mut asks = reader::<Ask>(recv);

    let mut control = false;
    let mut pending: Option<oneshot::Receiver<bool>> = None;
    let why = loop {
        tokio::select! {
            a = asks.recv() => match a {
                Some(Ask::Input(i)) if control => {
                    let _ = screen.input(i).await;
                }
                Some(Ask::Ack(n)) => {
                    acked_tx.send_replace(n);
                }
                Some(Ask::Keyframe) => key.notify_one(),
                Some(Ask::WantControl) if !control && pending.is_none() => {
                    let (reply, answer) = oneshot::channel();
                    events.send(Event::ControlRequest { id, reply });
                    pending = Some(answer);
                }
                Some(Ask::Bye) | None => break "They disconnected.".to_string(),
                Some(_) => {}
            },
            yes = async { pending.as_mut().expect("guarded").await }, if pending.is_some() => {
                pending = None;
                control = matches!(yes, Ok(true));
                send_msg(send, &Tell::Control(control)).await?;
            }
            c = cmds.recv() => match c {
                Some(HostCmd::Revoke) => {
                    control = false;
                    screen.release().await;
                    send_msg(send, &Tell::Control(false)).await?;
                }
                Some(HostCmd::Stop) | None => break "You stopped sharing.".to_string(),
            },
            r = &mut pump => break match r {
                Ok(Err(e)) => format!("Sharing stopped: {e}"),
                _ => "Sharing stopped.".to_string(),
            },
        }
    };
    pump.abort();
    Ok(why)
}

/// Capture -> encode -> send, newest frame only, at most 30 fps and `IN_FLIGHT` unacked.
async fn pump(
    mut frames: watch::Receiver<Option<Arc<Frame>>>,
    mut out: SendStream,
    mut acked: watch::Receiver<u32>,
    key: Arc<Notify>,
) -> anyhow::Result<()> {
    let mut enc = codec::Encoder::default();
    let mut seq = 0u32;
    let mut last: Option<Arc<Frame>> = None;
    loop {
        while seq.wrapping_sub(*acked.borrow_and_update()) >= IN_FLIGHT {
            acked.changed().await?;
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

// ---------------------------------------------------------------- viewing

/// A session to someone else's screen. Dropping it hangs up.
pub struct View {
    pub id: u64,
    tx: mpsc::UnboundedSender<Ask>,
    /// The newest decoded picture, taken by the window when it draws.
    pub frame: Arc<Mutex<Option<ColorImage>>>,
}

impl View {
    pub fn send(&self, a: Ask) {
        let _ = self.tx.send(a);
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.tx.send(Ask::Bye);
    }
}

pub fn view(ep: Endpoint, peer: EndpointId, name: String, events: Events) -> View {
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded_channel();
    let frame = Arc::new(Mutex::new(None));
    let (acks, slot) = (tx.clone(), frame.clone());
    tokio::spawn(async move {
        let why = match watch_screen(ep, peer, name, rx, acks, slot, &events, id).await {
            Ok(why) => why,
            Err(e) => e.to_string(),
        };
        events.send(Event::ViewEnded { id, why });
    });
    View { id, tx, frame }
}

#[allow(clippy::too_many_arguments)]
async fn watch_screen(
    ep: Endpoint,
    peer: EndpointId,
    name: String,
    mut rx: mpsc::UnboundedReceiver<Ask>,
    acks: mpsc::UnboundedSender<Ask>,
    slot: Arc<Mutex<Option<ColorImage>>>,
    events: &Events,
    id: u64,
) -> anyhow::Result<String> {
    let status = |text: &str| events.send(Event::Status { id, text: text.into() });
    status("Reaching their computer…");
    let conn = tokio::select! {
        c = timeout(Duration::from_secs(20), ep.connect(peer, ALPN)) => {
            c.map_err(|_| anyhow::anyhow!("Couldn't reach that computer. Is Deskline open there?"))??
        }
        _ = rx.recv() => return Ok(String::new()),
    };
    let (mut send, mut recv) = conn.open_bi().await?;
    send_msg(&mut send, &Ask::Hello { version: VERSION.into(), name }).await?;
    status("Waiting for them to accept…");

    let reply = tokio::select! {
        r = recv_msg::<Tell>(&mut recv) => r.map_err(|_| anyhow::anyhow!("They closed the connection."))?,
        _ = rx.recv() => {
            conn.close(0u8.into(), b"bye");
            return Ok(String::new());
        }
    };
    let who = match reply {
        Tell::Accepted { name } => clean(&name),
        Tell::Denied(why) => anyhow::bail!(why),
        Tell::Control(_) => anyhow::bail!("Unexpected reply."),
    };
    events.send(Event::Viewing { id, who });

    let video = conn.accept_uni().await?;
    let decoder = {
        let ctx = events.ctx.clone();
        tokio::spawn(async move { decode(video, acks, slot, ctx).await })
    };
    let mut tells = reader::<Tell>(recv);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let why = loop {
        tokio::select! {
            a = rx.recv() => match a {
                Some(Ask::Bye) | None => break String::new(),
                Some(a) => send_msg(&mut send, &a).await?,
            },
            t = tells.recv() => match t {
                Some(Tell::Control(on)) => events.send(Event::Control { id, on }),
                Some(_) => {}
                None => break "The session ended.".into(),
            },
            _ = tick.tick() => events.send(Event::Link { id, text: link(&conn) }),
        }
    };
    decoder.abort();
    conn.close(0u8.into(), b"bye");
    Ok(why)
}

async fn decode(
    mut video: RecvStream,
    acks: mpsc::UnboundedSender<Ask>,
    slot: Arc<Mutex<Option<ColorImage>>>,
    ctx: egui::Context,
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
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(_) => {
                let _ = acks.send(Ask::Keyframe);
            }
        }
        if acks.send(Ask::Ack(p.seq)).is_err() {
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------- wire

async fn send_msg<T: Serialize>(w: &mut SendStream, m: &T) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(m)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    Ok(())
}

async fn recv_msg<T: DeserializeOwned>(r: &mut RecvStream) -> anyhow::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(n <= 16 << 20, "message too large: {n} bytes");
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

/// Read messages on their own task: `recv_msg` must never be cancelled halfway through a frame.
fn reader<T: DeserializeOwned + Send + 'static>(mut r: RecvStream) -> mpsc::UnboundedReceiver<T> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(m) = recv_msg(&mut r).await {
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    rx
}

/// Names come from the other side: keep them short and printable.
fn clean(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(40).collect();
    if s.trim().is_empty() { "Someone".into() } else { s }
}

/// "LAN 3 ms", "Direct 41 ms" or "Relay 120 ms".
fn link(conn: &Connection) -> String {
    let paths = conn.paths();
    let Some(p) = paths.iter().find(|p| p.is_selected()) else { return "Relay".into() };
    let kind = match p.remote_addr() {
        TransportAddr::Ip(a) if is_lan(a.ip()) => "LAN",
        TransportAddr::Ip(_) => "Direct",
        _ => "Relay",
    };
    format!("{kind} {} ms", p.rtt().as_millis())
}

fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::imp::INJECTED;

    async fn next(rx: &mut mpsc::UnboundedReceiver<Event>, what: &str, want: impl Fn(&Event) -> bool) -> Event {
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

    /// Real endpoints on this machine, fake screen. Run with
    /// `cargo test two_peers -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn two_peers_request_accept_view_control_stop() {
        let dir = std::env::temp_dir().join(format!("deskline-test-{}", std::process::id()));
        let (home_a, home_b) = (dir.join("a"), dir.join("b"));
        let (a, b) = (start(&home_a).await.unwrap(), start(&home_b).await.unwrap());
        assert_eq!(start(&home_a).await.unwrap().id(), a.id(), "identity must survive a restart");
        assert_eq!(parse_code(&code(a.id())), Some(a.id()));

        let ctx = egui::Context::default();
        let (host_events, mut host) = Events::new(ctx.clone());
        let (view_events, mut viewer) = Events::new(ctx);
        listen(a.clone(), home_a, "host".into(), host_events);

        // Declined first.
        let v = view(b.clone(), a.id(), "guest".into(), view_events.clone());
        let Event::Request { who, reply } = next(&mut host, "request", |e| matches!(e, Event::Request { .. })).await else { unreachable!() };
        assert!(who.starts_with("guest ("), "{who}");
        reply.send(false).unwrap();
        let Event::ViewEnded { why, .. } = next(&mut viewer, "decline", |e| matches!(e, Event::ViewEnded { .. })).await else { unreachable!() };
        assert_eq!(why, "They declined.");
        drop(v);

        // Accepted: frames arrive and decode.
        let v = view(b.clone(), a.id(), "guest".into(), view_events);
        let Event::Request { reply, .. } = next(&mut host, "request", |e| matches!(e, Event::Request { .. })).await else { unreachable!() };
        reply.send(true).unwrap();
        let Event::Sharing { ctl, .. } = next(&mut host, "sharing", |e| matches!(e, Event::Sharing { .. })).await else { unreachable!() };
        next(&mut viewer, "viewing", |e| matches!(e, Event::Viewing { who, .. } if who == "host")).await;
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

        // Input is ignored until control is granted.
        v.send(Ask::Input(Input::Move { x: 0.1, y: 0.1 }));
        v.send(Ask::WantControl);
        let Event::ControlRequest { reply, .. } = next(&mut host, "control request", |e| matches!(e, Event::ControlRequest { .. })).await else { unreachable!() };
        reply.send(true).unwrap();
        next(&mut viewer, "control on", |e| matches!(e, Event::Control { on: true, .. })).await;
        v.send(Ask::Input(Input::Move { x: 0.5, y: 0.25 }));
        v.send(Ask::Input(Input::Key { code: 30, down: true }));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(*INJECTED.lock().unwrap(), vec![Input::Move { x: 0.5, y: 0.25 }, Input::Key { code: 30, down: true }]);

        // Taking control back releases the held key.
        ctl.send(HostCmd::Revoke).unwrap();
        next(&mut viewer, "control off", |e| matches!(e, Event::Control { on: false, .. })).await;
        assert_eq!(INJECTED.lock().unwrap().last(), Some(&Input::Key { code: 30, down: false }));

        ctl.send(HostCmd::Stop).unwrap();
        next(&mut viewer, "end", |e| matches!(e, Event::ViewEnded { .. })).await;
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// "user@host" for the other side's prompt.
pub fn whoami() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let user = var("USER").or_else(|| var("USERNAME")).unwrap_or_else(|| "someone".into());
    let host = var("COMPUTERNAME")
        .or_else(|| var("HOSTNAME"))
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string()))
        .filter(|h| !h.is_empty());
    match host {
        Some(h) => format!("{user}@{h}"),
        None => user,
    }
}
