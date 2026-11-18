//! wlroots compositors (Sway, Hyprland, river…) have no RemoteDesktop portal, so input goes
//! through their virtual pointer and virtual keyboard. The Wayland connection lives on its own
//! thread; `Devices` only hands it work.
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Instant;

use anyhow::Context;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::desktop::Input;

/// Absolute pointer positions are fractions of this.
const EXTENT: u32 = 10_000;

/// Used when the seat has no keyboard to copy (headless). The compositor fills the includes in
/// from the system's XKB files.
const US_KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compat { include "complete" };
    xkb_symbols { include "pc+us+inet(evdev)" };
};
"#;

enum Cmd {
    Input(Input),
    /// Pin the pointer to the output with this name (the portal stream's mapping id).
    MapTo(Option<String>),
}

pub struct Devices {
    cmds: mpsc::Sender<Cmd>,
}

impl Devices {
    /// Fails where the compositor has no virtual pointer or keyboard (not wlroots-based).
    pub fn connect() -> anyhow::Result<Self> {
        let (cmds, rx) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        std::thread::Builder::new().name("wayland".into()).spawn(move || match Wayland::connect() {
            Ok(w) => {
                let _ = ready_tx.send(Ok(()));
                w.run(rx);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        ready.recv().context("Wayland thread died")??;
        Ok(Self { cmds })
    }

    pub fn map_to(&self, output: Option<&str>) {
        let _ = self.cmds.send(Cmd::MapTo(output.map(str::to_owned)));
    }

    pub fn send(&self, i: Input) -> anyhow::Result<()> {
        self.cmds.send(Cmd::Input(i)).map_err(|_| anyhow::anyhow!("lost the connection to the compositor"))
    }
}

struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    seat: wl_seat::WlSeat,
    pointers: ZwlrVirtualPointerManagerV1,
    pointer: Option<ZwlrVirtualPointerV1>,
    keyboard: ZwpVirtualKeyboardV1,
    start: Instant,
}

impl Wayland {
    fn connect() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let pointers: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ())?;
        let keyboards: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ())?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ())?;
        // Output names (wl_output 4) let the pointer be pinned to the monitor being shared.
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" && g.version >= 4 {
                let _: wl_output::WlOutput = globals.registry().bind(g.name, 4, &qh, ());
            }
        }
        let mut state = State::default();
        queue.roundtrip(&mut state)?;

        // Typing follows this computer's own keyboard layout, copied from its seat.
        if state.has_keyboard {
            let kb = seat.get_keyboard(&qh, ());
            queue.roundtrip(&mut state)?;
            if kb.version() >= 3 {
                kb.release();
            }
        }
        let (fd, size) = match state.keymap.take() {
            Some(keymap) => keymap,
            None => us_keymap()?,
        };
        let keyboard = keyboards.create_virtual_keyboard(&seat, &qh, ());
        keyboard.keymap(1, fd.as_fd(), size); // 1: WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1
        // A compositor that refuses virtual input says so now rather than at the first key.
        queue.roundtrip(&mut state)?;
        Ok(Self { conn, queue, qh, state, seat, pointers, pointer: None, keyboard, start: Instant::now() })
    }

    fn run(mut self, cmds: mpsc::Receiver<Cmd>) {
        while let Ok(cmd) = cmds.recv() {
            match cmd {
                Cmd::MapTo(output) => self.map_to(output),
                Cmd::Input(i) => self.input(i),
            }
            if self.conn.flush().is_err() {
                break;
            }
            // Take in what the compositor sent (output changes and such) so it doesn't pile up.
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            if self.queue.dispatch_pending(&mut self.state).is_err() {
                break;
            }
        }
    }

    fn map_to(&mut self, output: Option<String>) {
        if let Some(old) = self.pointer.take() {
            old.destroy();
        }
        let output = output.and_then(|name| self.state.outputs.iter().find(|(_, n)| *n == name)).map(|(o, _)| o);
        self.pointer = Some(match output {
            Some(o) if self.pointers.version() >= 2 => {
                self.pointers.create_virtual_pointer_with_output(Some(&self.seat), Some(o), &self.qh, ())
            }
            // One monitor, or no way to tell which: absolute moves span the whole desktop.
            _ => self.pointers.create_virtual_pointer(Some(&self.seat), &self.qh, ()),
        });
    }

    fn input(&mut self, i: Input) {
        if self.pointer.is_none() {
            self.map_to(None);
        }
        let Some(p) = &self.pointer else { return };
        let time = self.start.elapsed().as_millis() as u32;
        match i {
            Input::Move { x, y } => {
                let at = |f: f32| (f.clamp(0.0, 1.0) * EXTENT as f32) as u32;
                p.motion_absolute(time, at(x), at(y), EXTENT, EXTENT);
                p.frame();
            }
            Input::Button { button, down } => {
                let Some(&b) = super::BUTTONS.get(button as usize) else { return };
                let state = if down { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
                p.button(time, b, state);
                p.frame();
            }
            // Wayland steps grow downward; ours grow upward. 15 is one wheel notch's distance.
            Input::Scroll { x, y } => {
                p.axis_source(wl_pointer::AxisSource::Wheel);
                for (axis, steps) in [(wl_pointer::Axis::VerticalScroll, -y), (wl_pointer::Axis::HorizontalScroll, x)] {
                    if steps != 0 {
                        p.axis_discrete(time, axis, steps as f64 * 15.0, steps);
                    }
                }
                p.frame();
            }
            Input::Key { code, down } => self.keyboard.key(time, code.into(), down.into()),
            Input::CtrlAltDel => {}
        }
    }
}

/// A US layout in a memory file, the way the protocol takes keymaps.
fn us_keymap() -> anyhow::Result<(OwnedFd, u32)> {
    // SAFETY: a NUL-terminated name in, a new fd (or -1) out.
    let fd = unsafe { libc::memfd_create(c"deskline-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    anyhow::ensure!(fd >= 0, "couldn't set up the keyboard: {}", std::io::Error::last_os_error());
    // SAFETY: the fd was just created and nothing else owns it.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(US_KEYMAP.as_bytes())?;
    file.write_all(&[0])?;
    Ok((file.into(), US_KEYMAP.len() as u32 + 1))
}

#[derive(Default)]
struct State {
    has_keyboard: bool,
    keymap: Option<(OwnedFd, u32)>,
    outputs: Vec<(wl_output::WlOutput, String)>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(state: &mut Self, _: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = event {
            state.has_keyboard = c.contains(wl_seat::Capability::Keyboard);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } = event {
            state.keymap = Some((fd, size));
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.outputs.push((output.clone(), name));
        }
    }
}

delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);
