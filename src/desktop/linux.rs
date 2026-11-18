//! Linux: shares the screen whichever way this desktop allows, best first:
//! - the xdg RemoteDesktop portal (GNOME, KDE; Wayland or X11): one session gives the picked
//!   monitors as PipeWire streams and accepts input;
//! - wlroots compositors (Sway, Hyprland, river…): the ScreenCast portal for the picture, the
//!   compositor's virtual pointer and keyboard for input (`wlr.rs`);
//! - X11 sessions without the portal (XFCE, Cinnamon, MATE, i3…): the X server itself (`x11.rs`).
//!
//! Portals ask the user once; the answer is remembered with a restore token so later sessions
//! only need Deskline's own prompt.
use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Context;
use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream};
use ashpd::desktop::{PersistMode, Session};
use pipewire as pw;
use pw::spa;
use tokio::sync::watch;

use super::{ALT, CTRL, Frame, Input, Monitor};

#[path = "wlr.rs"]
mod wlr;
#[path = "x11.rs"]
mod x11;

type Frames = watch::Receiver<Option<Arc<Frame>>>;

/// evdev BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE (back), BTN_EXTRA (forward): `Input::Button`'s
/// numbering.
const BUTTONS: [u32; 5] = [0x110, 0x111, 0x112, 0x113, 0x114];

const DELETE: u16 = 111;

/// Linux has no secure attention sequence; the keys themselves at least reach a lock screen or
/// logout dialog.
const CTRL_ALT_DEL: &[Input] = &[
    Input::Key { code: CTRL, down: true },
    Input::Key { code: ALT, down: true },
    Input::Key { code: DELETE, down: true },
    Input::Key { code: DELETE, down: false },
    Input::Key { code: ALT, down: false },
    Input::Key { code: CTRL, down: false },
];

pub struct Screen(Backend);

enum Backend {
    Portal { portal: RemoteDesktop, session: Session<RemoteDesktop>, capture: Capture },
    Wlr { session: Session<Screencast>, capture: Capture, devices: wlr::Devices },
    X11(x11::Screen),
}

impl Screen {
    pub async fn open(home: &Path) -> anyhow::Result<(Self, Frames)> {
        if let Some(portal) = remote_desktop().await {
            return open_portal(home, portal).await;
        }
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        // Wayland without the RemoteDesktop portal: wlroots, if the compositor offers virtual input.
        if wayland && let Ok(devices) = tokio::task::block_in_place(wlr::Devices::connect) {
            return open_wlr(home, devices).await;
        }
        let x11 = std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("x11") || !wayland;
        if x11 && std::env::var_os("DISPLAY").is_some() {
            let (screen, frames) = tokio::task::block_in_place(x11::Screen::open)?;
            return Ok((Self(Backend::X11(screen)), frames));
        }
        anyhow::bail!(
            "This desktop has no screen sharing support Deskline can use (needs xdg-desktop-portal RemoteDesktop, wlroots, or X11)."
        )
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        for &i in expand(&i) {
            match &mut self.0 {
                Backend::Portal { portal, session, capture } => portal_input(portal, session, capture, i).await?,
                Backend::Wlr { devices, .. } => devices.send(i)?,
                Backend::X11(x) => x.input(i)?,
            }
        }
        Ok(())
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        match &self.0 {
            Backend::Portal { capture, .. } | Backend::Wlr { capture, .. } => capture.monitors(),
            Backend::X11(x) => x.monitors(),
        }
    }

    pub fn current(&self) -> usize {
        match &self.0 {
            Backend::Portal { capture, .. } | Backend::Wlr { capture, .. } => capture.current,
            Backend::X11(x) => x.current(),
        }
    }

    pub async fn select(&mut self, index: usize) -> anyhow::Result<()> {
        match &mut self.0 {
            Backend::Portal { capture, .. } => capture.select(index),
            Backend::Wlr { capture, devices, .. } => {
                capture.select(index)?;
                devices.map_to(capture.stream().mapping_id());
                Ok(())
            }
            Backend::X11(x) => x.select(index),
        }
    }

    pub async fn privacy(&mut self, on: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!on, "Privacy mode isn't available on Linux.");
        Ok(())
    }

    pub async fn close(self) {
        match self.0 {
            Backend::Portal { session, .. } => {
                let _ = session.close().await;
            }
            Backend::Wlr { session, .. } => {
                let _ = session.close().await;
            }
            Backend::X11(_) => {}
        }
    }
}

/// Ctrl+Alt+Del becomes its three keys; everything else passes through.
fn expand(i: &Input) -> &[Input] {
    if *i == Input::CtrlAltDel { CTRL_ALT_DEL } else { std::slice::from_ref(i) }
}

/// The RemoteDesktop portal, when this desktop has one that drives both pointer and keyboard.
async fn remote_desktop() -> Option<RemoteDesktop> {
    let portal = RemoteDesktop::new().await.ok()?;
    let devices = portal.available_device_types().await.ok()?;
    devices.contains(DeviceType::KEYBOARD | DeviceType::POINTER).then_some(portal)
}

/// The ScreenCast portal, when it can share monitors.
async fn screencast() -> Option<Screencast> {
    let portal = Screencast::new().await.ok()?;
    portal.available_source_types().await.ok()?.contains(SourceType::MONITOR).then_some(portal)
}

async fn open_portal(home: &Path, portal: RemoteDesktop) -> anyhow::Result<(Screen, Frames)> {
    let cast = Screencast::new().await.context("no screencast portal (xdg-desktop-portal)")?;
    let session = portal.create_session(Default::default()).await?;
    let token_file = home.join("portal-token");
    let token = std::fs::read_to_string(&token_file).ok();
    portal
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::KEYBOARD | DeviceType::POINTER)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?;
    // Several monitors may be picked; the viewer switches between them with `select`.
    cast.select_sources(
        &session,
        SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::EMBEDDED)
            .set_sources(SourceType::MONITOR)
            .set_multiple(true),
    )
    .await?;
    let started = portal
        .start(&session, None, Default::default())
        .await?
        .response()
        .map_err(|_| anyhow::anyhow!("screen sharing was cancelled"))?;
    if let Some(t) = started.restore_token() {
        let _ = std::fs::write(&token_file, t);
    }
    let fd = cast.open_pipe_wire_remote(&session, Default::default()).await?;
    let (capture, frames) = Capture::start(fd, started.streams().to_vec())?;
    Ok((Screen(Backend::Portal { portal, session, capture }), frames))
}

async fn open_wlr(home: &Path, devices: wlr::Devices) -> anyhow::Result<(Screen, Frames)> {
    let cast = screencast()
        .await
        .context("sharing this screen needs xdg-desktop-portal-wlr (or your compositor's own portal) running")?;
    let session = cast.create_session(Default::default()).await?;
    let token_file = home.join("screencast-token");
    let token = std::fs::read_to_string(&token_file).ok();
    cast.select_sources(
        &session,
        SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::EMBEDDED)
            .set_sources(SourceType::MONITOR)
            .set_multiple(true)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(token.as_deref()),
    )
    .await?;
    let started = cast
        .start(&session, None, Default::default())
        .await?
        .response()
        .map_err(|_| anyhow::anyhow!("screen sharing was cancelled"))?;
    if let Some(t) = started.restore_token() {
        let _ = std::fs::write(&token_file, t);
    }
    let fd = cast.open_pipe_wire_remote(&session, Default::default()).await?;
    let (capture, frames) = Capture::start(fd, started.streams().to_vec())?;
    devices.map_to(capture.stream().mapping_id());
    Ok((Screen(Backend::Wlr { session, capture, devices }), frames))
}

async fn portal_input(portal: &RemoteDesktop, s: &Session<RemoteDesktop>, capture: &Capture, i: Input) -> anyhow::Result<()> {
    let state = |down| if down { KeyState::Pressed } else { KeyState::Released };
    match i {
        Input::Move { x, y } => {
            let Some((w, h)) = capture.size() else { return Ok(()) };
            let (x, y) = (x.clamp(0.0, 1.0) as f64 * w, y.clamp(0.0, 1.0) as f64 * h);
            let node = capture.stream().pipe_wire_node_id();
            portal.notify_pointer_motion_absolute(s, node, x, y, Default::default()).await?
        }
        Input::Button { button, down } => {
            let Some(&code) = BUTTONS.get(button as usize) else { return Ok(()) };
            portal.notify_pointer_button(s, code as i32, state(down), Default::default()).await?
        }
        // Portal steps grow downward; ours grow upward.
        Input::Scroll { x, y } => {
            if y != 0 {
                portal.notify_pointer_axis_discrete(s, Axis::Vertical, -y, Default::default()).await?
            }
            if x != 0 {
                portal.notify_pointer_axis_discrete(s, Axis::Horizontal, x, Default::default()).await?
            }
        }
        Input::Key { code, down } => {
            portal.notify_keyboard_keycode(s, code as i32, state(down), Default::default()).await?
        }
        Input::CtrlAltDel => {}
    }
    Ok(())
}

/// The monitors a portal session shares and the PipeWire thread showing one of them.
struct Capture {
    streams: Vec<Stream>,
    current: usize,
    cmds: pw::channel::Sender<Cmd>,
    frames: Frames,
}

enum Cmd {
    Switch(u32),
    Stop,
}

impl Capture {
    fn start(fd: OwnedFd, streams: Vec<Stream>) -> anyhow::Result<(Self, Frames)> {
        let node = streams.first().context("no screen was picked")?.pipe_wire_node_id();
        let (frames_tx, frames) = watch::channel(None);
        let (cmds, cmds_rx) = pw::channel::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("pipewire".into()).spawn(move || {
            if let Err(e) = capture(fd, node, frames_tx, cmds_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        // Wait for the stream to be set up so a failure is reported instead of a black screen.
        tokio::task::block_in_place(|| ready.recv()).context("capture thread died")??;
        Ok((Self { streams, current: 0, cmds, frames: frames.clone() }, frames))
    }

    fn stream(&self) -> &Stream {
        &self.streams[self.current]
    }

    /// The shared monitor's size in the compositor's (logical) coordinates, for absolute pointer
    /// moves. Without one from the portal, the picture's size is the best guess.
    fn size(&self) -> Option<(f64, f64)> {
        let frame = || self.frames.borrow().as_ref().map(|f| (f.w as f64, f.h as f64));
        self.stream().size().map(|(w, h)| (w as f64, h as f64)).or_else(frame)
    }

    fn monitors(&self) -> Vec<Monitor> {
        let monitor = |(i, s): (usize, &Stream)| {
            let (w, h) = s.size().unwrap_or_default();
            Monitor { name: format!("Screen {}", i + 1), w: w.max(0) as u32, h: h.max(0) as u32, primary: i == 0 }
        };
        self.streams.iter().enumerate().map(monitor).collect()
    }

    fn select(&mut self, index: usize) -> anyhow::Result<()> {
        let node = self.streams.get(index).context("that screen isn't shared")?.pipe_wire_node_id();
        if index != self.current {
            self.cmds.send(Cmd::Switch(node)).map_err(|_| anyhow::anyhow!("screen capture has stopped"))?;
            self.current = index;
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Stop);
    }
}

/// Runs the PipeWire loop on this thread until `Stop`, capturing whichever node `Switch` named
/// last. When the system ends the stream (its own "sharing" indicator), the loop ends and drops
/// the frame sender, which ends the session.
fn capture(
    fd: OwnedFd,
    node: u32,
    frames: watch::Sender<Option<Arc<Frame>>>,
    cmds: pw::channel::Receiver<Cmd>,
    ready: &std::sync::mpsc::Sender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let frames = Rc::new(frames);
    let watching = Rc::new(RefCell::new(Some(connect(&core, &mainloop, node, frames.clone())?)));
    let _cmds = cmds.attach(mainloop.loop_(), {
        let (mainloop, watching) = (mainloop.clone(), watching.clone());
        move |cmd| match cmd {
            Cmd::Stop => mainloop.quit(),
            Cmd::Switch(node) => {
                drop(watching.take());
                match connect(&core, &mainloop, node, frames.clone()) {
                    Ok(w) => *watching.borrow_mut() = Some(w),
                    Err(_) => mainloop.quit(),
                }
            }
        }
    });
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// One monitor's stream. The listener is declared first so it drops first: the stream's own
/// disconnect on a monitor switch then can't end the session.
struct Watching {
    _listener: pw::stream::StreamListener<Data>,
    _stream: pw::stream::StreamRc,
}

struct Data {
    info: spa::param::video::VideoInfoRaw,
    frames: Rc<watch::Sender<Option<Arc<Frame>>>>,
}

fn connect(
    core: &pw::core::CoreRc,
    mainloop: &pw::main_loop::MainLoopRc,
    node: u32,
    frames: Rc<watch::Sender<Option<Arc<Frame>>>>,
) -> anyhow::Result<Watching> {
    use spa::param::format::{MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::pod::Pod;

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "deskline",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let quit = mainloop.clone();
    let listener = stream
        .add_local_listener_with_user_data(Data { info: Default::default(), frames })
        .state_changed(move |_, _, _, new| {
            if matches!(new, pw::stream::StreamState::Error(_) | pw::stream::StreamState::Unconnected) {
                quit.quit();
            }
        })
        .param_changed(|_, s, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Ok((MediaType::Video, MediaSubtype::Raw)) = spa::param::format_utils::parse_format(param) {
                let _ = s.info.parse(param);
            }
        })
        .process(|stream, s| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(d) = buffer.datas_mut().first_mut() else { return };
            let (w, h) = (s.info.size().width as usize, s.info.size().height as usize);
            let chunk = d.chunk();
            let (offset, size) = (chunk.offset() as usize, chunk.size() as usize);
            let stride = match chunk.stride() {
                n if n > 0 => n as usize,
                _ => w * 4,
            };
            let bgra = matches!(s.info.format(), VideoFormat::BGRx | VideoFormat::BGRA);
            let Some(src) = d.data() else { return };
            if w == 0 || h == 0 || size == 0 || src.len() < offset + (h - 1) * stride + w * 4 {
                return;
            }
            let mut px = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                px.extend_from_slice(&src[offset + y * stride..][..w * 4]);
            }
            s.frames.send_replace(Some(Arc::new(Frame { w, h, bgra, px })));
        })
        .register()?;

    let bytes = format_pod()?;
    let mut params = [Pod::from_bytes(&bytes).context("bad format pod")?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok(Watching { _listener: listener, _stream: stream })
}

/// Plain memory buffers (no DMA-BUF modifiers offered), 4-byte pixels, at most 30 fps.
fn format_pod() -> anyhow::Result<Vec<u8>> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::pod::{Value, serialize::PodSerializer};

    let format = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 30, denom: 1 }
        ),
    );
    Ok(PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(format))?.0.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_alt_del_presses_then_releases_in_reverse() {
        let keys: Vec<_> = expand(&Input::CtrlAltDel)
            .iter()
            .map(|i| match *i {
                Input::Key { code, down } => (code, down),
                other => panic!("not a key: {other:?}"),
            })
            .collect();
        assert_eq!(keys, [(29, true), (56, true), (111, true), (111, false), (56, false), (29, false)]);
        let click = Input::Button { button: 0, down: true };
        assert_eq!(expand(&click), [click]);
    }

    #[test]
    fn back_and_forward_are_side_and_extra() {
        assert_eq!(BUTTONS[3], 0x113);
        assert_eq!(BUTTONS[4], 0x114);
    }
}
