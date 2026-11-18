//! Linux: one xdg RemoteDesktop portal session gives both the screen (a PipeWire stream) and
//! input injection. Works on GNOME and KDE, Wayland or X11. The system asks the user once; the
//! answer is remembered with a restore token so later sessions only need Deskline's own prompt.
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use pipewire as pw;
use pw::spa;
use tokio::sync::watch;

use super::{Frame, Input};

pub struct Screen {
    portal: RemoteDesktop,
    session: Session<RemoteDesktop>,
    node: u32,
    /// The stream's size in the compositor's (logical) coordinates, for absolute pointer moves.
    size: (f64, f64),
    stop: pw::channel::Sender<()>,
}

impl Screen {
    pub async fn open(home: &Path) -> anyhow::Result<(Self, watch::Receiver<Option<Arc<Frame>>>)> {
        let portal = RemoteDesktop::new().await.context("no remote desktop portal (xdg-desktop-portal)")?;
        let cast = Screencast::new().await.context("no screencast portal (xdg-desktop-portal)")?;
        let session = portal.create_session(Default::default()).await?;
        let token_file: PathBuf = home.join("portal-token");
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
        cast.select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::EMBEDDED)
                .set_sources(SourceType::MONITOR)
                .set_multiple(false),
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
        let stream = started.streams().first().context("no screen was picked")?;
        let node = stream.pipe_wire_node_id();
        let size = stream.size().map(|(w, h)| (w as f64, h as f64));
        let fd = cast.open_pipe_wire_remote(&session, Default::default()).await?;

        let (frames_tx, frames) = watch::channel(None);
        let (stop, stop_rx) = pw::channel::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("pipewire".into()).spawn(move || {
            if let Err(e) = capture(fd, node, frames_tx, stop_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        // Wait for the stream to be set up so a failure is reported instead of a black screen.
        tokio::task::block_in_place(|| ready.recv()).context("capture thread died")??;

        // Without a size from the portal, the first frame's size is the best guess.
        let size = match size {
            Some(s) => s,
            None => {
                let mut f = frames.clone();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), f.changed()).await;
                f.borrow().as_ref().map_or((1920.0, 1080.0), |f| (f.w as f64, f.h as f64))
            }
        };
        Ok((Self { portal, session, node, size, stop }, frames))
    }

    pub async fn input(&mut self, i: Input) -> anyhow::Result<()> {
        let state = |down| if down { KeyState::Pressed } else { KeyState::Released };
        let (p, s) = (&self.portal, &self.session);
        match i {
            Input::Move { x, y } => {
                let (x, y) = (x.clamp(0.0, 1.0) as f64 * self.size.0, y.clamp(0.0, 1.0) as f64 * self.size.1);
                p.notify_pointer_motion_absolute(s, self.node, x, y, Default::default()).await?
            }
            // evdev BTN_LEFT, BTN_RIGHT, BTN_MIDDLE
            Input::Button { button, down } => {
                let code = [0x110, 0x111, 0x112][button.min(2) as usize];
                p.notify_pointer_button(s, code, state(down), Default::default()).await?
            }
            // Portal steps grow downward; ours grow upward.
            Input::Scroll { x, y } => {
                if y != 0 {
                    p.notify_pointer_axis_discrete(s, Axis::Vertical, -y, Default::default()).await?
                }
                if x != 0 {
                    p.notify_pointer_axis_discrete(s, Axis::Horizontal, x, Default::default()).await?
                }
            }
            Input::Key { code, down } => p.notify_keyboard_keycode(s, code as i32, state(down), Default::default()).await?,
            Input::CtrlAltDel => {}
        }
        Ok(())
    }

    pub fn monitors(&self) -> Vec<super::Monitor> {
        vec![super::Monitor { name: "Screen".into(), w: self.size.0 as u32, h: self.size.1 as u32, primary: true }]
    }

    pub fn current(&self) -> usize {
        0
    }

    pub async fn select(&mut self, _index: usize) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn privacy(&mut self, on: bool) -> anyhow::Result<()> {
        anyhow::ensure!(!on, "privacy mode isn't available on Linux");
        Ok(())
    }

    pub async fn close(self) {
        let _ = self.stop.send(());
        let _ = self.session.close().await;
    }
}

struct Stream {
    info: spa::param::video::VideoInfoRaw,
    frames: watch::Sender<Option<Arc<Frame>>>,
}

/// Runs the PipeWire loop on this thread until `stop` fires.
fn capture(
    fd: OwnedFd,
    node: u32,
    frames: watch::Sender<Option<Arc<Frame>>>,
    stop: pw::channel::Receiver<()>,
    ready: &std::sync::mpsc::Sender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::pod::{Pod, Value, serialize::PodSerializer};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "deskline",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let quit = mainloop.clone();
    let _stop = stop.attach(mainloop.loop_(), move |_| quit.quit());

    // Stopped from the system's own "sharing" indicator: end the loop, which drops the frame
    // sender and ends the session.
    let quit = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(Stream { info: Default::default(), frames })
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

    // Plain memory buffers (no DMA-BUF modifiers offered), 4-byte pixels, at most 30 fps.
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
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(format))?.0.into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("bad format pod")?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}
