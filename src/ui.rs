//! The window. Two jobs, two people: the person being helped sees their code, big, and one
//! question when someone wants in, then a red bar for as long as anyone is; the helper pastes a
//! code, waits on a line of dusk being drawn, and works in a tab per screen. Built from
//! luannzin.com's quiet system (look.rs), in English or Portuguese (i18n.rs).
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Align, Color32, CursorGrab, Event as UiEvent, Id, Key, Layout, Modal, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    RichText, ScrollArea, Sense, TextEdit, TextureHandle, TextureOptions, UiKind, Vec2, ViewportCommand, WidgetInfo, Role,
    scroll_area::ScrollSource, vec2,
};
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;

use crate::codec::Quality;
use crate::desktop::{self, ALT, CTRL, Input, Monitor, SHIFT};
use crate::files::Listing;
use crate::host::{HostCmd, HostEvent};
use crate::i18n::{t, tf};
use crate::look::{self, GREEN, GROUND, HAIRLINE, INK, RED, SOFT, muted, primary, primary_enabled, quiet, soft, strong};
#[cfg(target_os = "macos")]
use crate::permissions::Access;
use crate::proto::Ask;
use crate::settings::{self, Peer, Store};
use crate::tray::{Tray, TrayMsg};
use crate::update::{self, Updater};
use crate::viewer::{self, Cmd, Options, Out, Transfer, View, ViewEvent, What};
use crate::{autostart, net, short};
#[cfg(windows)]
use crate::{keyhook, service};

/// What main.rs hands the window.
pub struct Start {
    pub ep: Endpoint,
    pub home: PathBuf,
    pub store: Store,
    pub host: mpsc::UnboundedSender<HostCmd>,
    pub events: mpsc::UnboundedReceiver<HostEvent>,
    /// The host side is the installed Windows service.
    pub service: bool,
    pub background: bool,
    pub settings: bool,
    /// From the command line: connect to this code, maybe with its password.
    pub connect: Option<(String, Option<String>)>,
}

/// The reading column, like luannzin.com's.
const COLUMN: f32 = 560.0;
/// A file or folder picked to send: the session, the path here, the folder of theirs it goes to.
type Picked = (u64, PathBuf, Option<String>);
/// Finished transfers kept in the Files panel, newest first.
const FINISHED: usize = 5;
/// The starting places' names as hosts send them, and how they read here.
const PLACES: [(&str, &str); 4] = [("Home", "Home folder"), ("Desktop", "Desktop"), ("Documents", "Documents"), ("Downloads", "Downloads")];
/// Every picture setting, in the order they're offered.
const QUALITIES: [Quality; 4] = [Quality::Balanced, Quality::Sharp, Quality::Smooth, Quality::Gaming];
/// The evdev code of Z: Ctrl+Alt+Shift+Z gives a captured mouse back (the Windows keyboard hook
/// sees codes; egui sees `Key::Z`).
#[cfg(windows)]
const RELEASE_KEY: u16 = 44;

pub struct App {
    ep: Endpoint,
    store: Store,
    name: String,
    host: mpsc::UnboundedSender<HostCmd>,
    host_events: mpsc::UnboundedReceiver<HostEvent>,
    service: bool,
    out: Out,
    view_events: mpsc::UnboundedReceiver<(u64, ViewEvent)>,
    /// Files and folders picked to send: the session, the path, the folder of theirs it goes to.
    picked: (mpsc::UnboundedSender<Picked>, mpsc::UnboundedReceiver<Picked>),
    /// This computer's code, the long one: its key.
    code: String,
    short: Short,
    /// Their short code, being looked up.
    lookup: Option<Lookup>,
    password_on: bool,
    trusted: Vec<Peer>,
    partner: String,
    /// The last attempt said their unattended password would get us in.
    need_password: bool,
    password: String,
    remember: bool,
    note: String,
    /// When "Copy code" was last clicked: the button says "Copied" for 2 s.
    copied_at: Option<f64>,
    /// The note is this error: it offers Report this.
    trouble: Option<String>,
    report: Report,
    #[cfg(target_os = "macos")]
    mac: Mac,
    requests: Vec<Request>,
    shares: Vec<Share>,
    views: Vec<Viewing>,
    tab: Tab,
    form: Form,
    renaming: Option<(String, String)>,
    /// The contact just forgotten and where it was, until Undo or the page changes.
    forgotten: Option<(usize, settings::Contact)>,
    tray: Option<Tray>,
    hidden: bool,
    quitting: bool,
    updater: Option<Updater>,
    /// The pixel-art mark, drawn with nearest-neighbour so it stays crisp.
    mark: TextureHandle,
    /// The mouse is held by the window for a viewer (`Viewing::captured`) right now.
    grabbed: bool,
    #[cfg(windows)]
    keys: std::sync::mpsc::Receiver<(u16, bool)>,
    /// Ctrl, Alt and Shift as the keyboard hook saw them (bits 1, 2, 4).
    #[cfg(windows)]
    hook_mods: u8,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Home,
    Settings,
    View(u64),
}

/// This run's short code (short.rs): the digits Home shows, the code its record points at while
/// the window is open, and whether the last try to publish it worked (true before any try).
struct Short {
    shown: String,
    target: std::sync::mpsc::Sender<Option<EndpointId>>,
    sent: Option<EndpointId>,
    worked: Arc<AtomicBool>,
}

/// Their short code being looked up, and the password and tick to connect with once it's found.
struct Lookup {
    password: Option<String>,
    remember: bool,
    answer: std::sync::mpsc::Receiver<anyhow::Result<Option<EndpointId>>>,
}

struct Request {
    req: u64,
    who: String,
    control: bool,
    trust: bool,
}

struct Share {
    id: u64,
    who: String,
    control: bool,
    asking: bool,
    /// They asked for other screens (Linux): the system's dialog opens from a click here.
    screens: bool,
    chat: Chat,
}

#[derive(Default)]
struct Chat {
    /// (mine, text)
    lines: Vec<(bool, String)>,
    draft: String,
    open: bool,
    unread: usize,
}

#[derive(PartialEq)]
enum Control {
    Off,
    Asked,
    On,
}

#[derive(PartialEq, Clone, Copy)]
enum Panel {
    None,
    Files,
    Chat,
}

struct Viewing {
    session: View,
    peer: EndpointId,
    who: Option<String>,
    status: String,
    link: Option<(&'static str, u32)>,
    control: Control,
    tex: Option<TextureHandle>,
    /// Pictures shown in the last second.
    fps: u32,
    mods: Modifiers,
    scroll: Vec2,
    /// The mouse moves their pointer by raw counts (for games), since this time.
    captured: Option<f64>,
    /// Movement under one count, kept for the next.
    motion: Vec2,
    monitors: Vec<Monitor>,
    current: usize,
    privacy: bool,
    sound: bool,
    quality: Quality,
    actual_size: bool,
    fullscreen: bool,
    panel: Panel,
    chat: Chat,
    listing: Option<Listing>,
    /// Typed in Find: narrows the folder's rows.
    find: String,
    /// Under way first, then the last few finished.
    transfers: Vec<Transfer>,
    /// Their computer takes files into any folder and on its clipboard.
    tools: bool,
    /// They copied files too big to come over unasked: how many, how many bytes.
    their_files: Option<(usize, u64)>,
    notes: Vec<String>,
    /// Typed while waiting to be accepted: log in with their password instead.
    password: String,
    remember: bool,
}

/// Report a problem, in Settings.
#[derive(Default)]
struct Report {
    text: String,
    /// Report this was clicked: the field takes focus and scrolls into view.
    focus: bool,
    copied_at: Option<f64>,
}

/// macOS: Screen Recording and Accessibility (`Access::ALL`'s order), as last read and when, and
/// whether this run asked for each yet.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Mac {
    granted: [bool; 2],
    checked: Option<f64>,
    asked: [bool; 2],
}

#[derive(Default)]
struct Form {
    pw: String,
    pw2: String,
    relay: String,
    error: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Start) -> Self {
        let ctx = cc.egui_ctx.clone();
        look::install(&ctx);
        let Start { ep, home, store, host, mut events, service, background, settings, connect } = start;
        // Host events wake the window, even while it's hidden in the tray.
        let (host_tx, host_events) = mpsc::unbounded_channel();
        let wake = ctx.clone();
        tokio::spawn(async move {
            while let Some(e) = events.recv().await {
                if host_tx.send(e).is_err() {
                    break;
                }
                wake.request_repaint();
            }
        });
        let (view_tx, view_events) = mpsc::unbounded_channel();
        let relay = store.get().relay.unwrap_or_default();
        let tray = if service { None } else { Tray::start(&ctx) };
        // Started in the background with no tray to come back from (GNOME without the
        // AppIndicator extension, or the service's window): minimized instead of invisible.
        let hidden = background && (tray.is_some() || service);
        if background && !hidden {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
        let short = {
            let (target, targets) = std::sync::mpsc::channel();
            let (shown, worked) = (short::generate(), Arc::new(AtomicBool::new(true)));
            let (digits, last, wake) = (shown.clone(), worked.clone(), ctx.clone());
            std::thread::spawn(move || {
                short::publish(&digits, targets, |ok| {
                    last.store(ok, Ordering::Relaxed);
                    wake.request_repaint();
                })
            });
            Short { shown, target, sent: None, worked }
        };
        let mut app = Self {
            name: net::whoami(),
            code: net::code(ep.id()),
            short,
            lookup: None,
            ep,
            store,
            host,
            host_events,
            service,
            out: Out::new(view_tx, Some(ctx.clone())),
            view_events,
            picked: mpsc::unbounded_channel(),
            password_on: false,
            trusted: Vec::new(),
            partner: String::new(),
            need_password: false,
            password: String::new(),
            remember: false,
            note: String::new(),
            copied_at: None,
            trouble: None,
            report: Report::default(),
            #[cfg(target_os = "macos")]
            mac: Mac::default(),
            requests: Vec::new(),
            shares: Vec::new(),
            views: Vec::new(),
            tab: if settings { Tab::Settings } else { Tab::Home },
            form: Form { relay, ..Default::default() },
            renaming: None,
            forgotten: None,
            tray,
            hidden,
            quitting: false,
            updater: (!service).then(|| Updater::start(home.join("update"))),
            mark: {
                let img = image::load_from_memory(include_bytes!("../assets/tray.png")).expect("the embedded mark is a png").to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                ctx.load_texture("mark", egui::ColorImage::from_rgba_unmultiplied(size, &img), TextureOptions::NEAREST)
            },
            grabbed: false,
            #[cfg(windows)]
            keys: keyhook::start(ctx),
            #[cfg(windows)]
            hook_mods: 0,
        };
        if let Some((code, password)) = connect {
            app.partner = code.clone();
            app.connect(&code, password, false);
        }
        app
    }

    /// Bring the window up: someone is asking for something.
    fn show(&mut self, ctx: &egui::Context) {
        if self.hidden {
            self.hidden = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        }
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
    }

    fn host_event(&mut self, ctx: &egui::Context, e: HostEvent) {
        match e {
            HostEvent::Reset => {
                self.requests.clear();
                self.shares.clear();
            }
            HostEvent::Code(c) => self.code = c,
            HostEvent::Security { password, trusted } => {
                self.password_on = password;
                self.trusted = trusted;
            }
            HostEvent::Request { req, who } => {
                let control = self.store.get().allow_control;
                self.requests.retain(|r| r.req != req);
                self.requests.push(Request { req, who, control, trust: false });
                self.show(ctx);
            }
            HostEvent::RequestGone { req } => self.requests.retain(|r| r.req != req),
            HostEvent::Sharing { id, who, control } => {
                self.shares.retain(|s| s.id != id);
                self.shares.push(Share { id, who, control, asking: false, screens: false, chat: Chat::default() });
                // Someone got in (perhaps with the password, perhaps trusted): the red bar shows.
                self.show(ctx);
            }
            HostEvent::ControlRequest { id } => {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    s.asking = true;
                }
                self.show(ctx);
            }
            HostEvent::ScreensRequest { id } => {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    s.screens = true;
                }
                self.show(ctx);
            }
            HostEvent::Control { id, on } => {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    (s.control, s.asking) = (on, false);
                }
            }
            HostEvent::Chat { id, text } => {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    s.chat.lines.push((false, text));
                    if !s.chat.open {
                        s.chat.unread += 1;
                    }
                }
            }
            HostEvent::Note { id, text } => {
                let who = self.shares.iter().find(|s| s.id == id).map_or("", |s| s.who.as_str());
                self.note = format!("{who}: {}", t(&text));
            }
            HostEvent::ShareEnded { id, why } => {
                self.shares.retain(|s| s.id != id);
                self.note = t(&why);
            }
            HostEvent::Error(e) => {
                self.form.error = t(&e);
                self.note = t(&e);
                self.trouble = Some(self.note.clone());
            }
        }
    }

    fn view_event(&mut self, ctx: &egui::Context, id: u64, e: ViewEvent) {
        let Some(i) = self.views.iter().position(|v| v.session.id == id) else { return };
        let v = &mut self.views[i];
        match e {
            ViewEvent::Status(s) => {
                v.status = t(&s);
                v.link = None;
            }
            ViewEvent::Viewing { who, control } => {
                let _ = self.store.met(&v.peer.to_z32(), &who);
                v.who = Some(who);
                v.status.clear();
                v.control = if control { Control::On } else { Control::Off };
                if self.tab == Tab::View(id) {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                }
            }
            ViewEvent::Control(on) => {
                if !on && v.control == Control::Asked {
                    v.note(&t("They said no to control."));
                }
                v.control = if on { Control::On } else { Control::Off };
                if !on {
                    v.captured = None;
                }
            }
            ViewEvent::Monitors { list, current } => (v.monitors, v.current) = (list, current),
            ViewEvent::Link { kind, ms, fps } => {
                (v.link, v.fps) = (Some((kind, ms)), fps);
                v.status.clear();
            }
            ViewEvent::Chat(text) => {
                v.chat.lines.push((false, text));
                if v.panel != Panel::Chat {
                    v.chat.unread += 1;
                }
            }
            ViewEvent::Note(text) => v.note(&t(&text)),
            ViewEvent::Privacy(on) => v.privacy = on,
            ViewEvent::Listing(l) => {
                if v.listing.as_ref().is_none_or(|old| old.path != l.path) {
                    v.find.clear();
                }
                v.listing = Some(l);
            }
            ViewEvent::Transfer(tr) => {
                // Something sent may have landed in the folder on show.
                let landed = matches!(tr.what, What::Send(_)) && matches!(tr.end, Some(Ok(_)));
                if let (true, Some(l)) = (landed, &v.listing) {
                    v.session.cmd(Cmd::List(l.path.clone()));
                }
                match v.transfers.iter_mut().find(|x| x.id == tr.id) {
                    Some(x) => *x = tr,
                    None => v.transfers.insert(0, tr),
                }
                // Under way first, then the newest finished; the oldest finished go.
                v.transfers.sort_by_key(|x| x.end.is_some());
                let running = v.transfers.iter().filter(|x| x.end.is_none()).count();
                v.transfers.truncate(running + FINISHED);
            }
            ViewEvent::TheirFiles { count, bytes } => v.their_files = (count > 0).then_some((count, bytes)),
            ViewEvent::FileTools => v.tools = true,
            ViewEvent::Ended { why, password } => {
                let peer = v.peer;
                self.views.remove(i);
                if self.tab == Tab::View(id) {
                    self.tab = Tab::Home;
                }
                if self.views.is_empty() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
                }
                if password {
                    self.need_password = true;
                    self.partner = net::code(peer);
                }
                self.note = t(&why);
                self.trouble = (!why.is_empty()).then(|| self.note.clone());
            }
        }
    }

    fn connect(&mut self, code: &str, password: Option<String>, remember: bool) {
        if let Some(digits) = short::parse(code) {
            let (tx, answer) = std::sync::mpsc::channel();
            std::thread::spawn(move || tx.send(short::find(&digits)));
            self.lookup = Some(Lookup { password, remember, answer });
            (self.need_password, self.note) = (false, t("Looking up their code…"));
            return;
        }
        let Some(peer) = net::parse_code(code) else {
            self.note = t("That code doesn't look right. It's 9 digits, or 52 letters and digits.");
            return;
        };
        if peer == self.ep.id() || self.code.replace(' ', "") == peer.to_z32() {
            self.note = t("That's this computer's code. Paste your partner's.");
            return;
        }
        let s = self.store.get();
        let opts = Options { name: self.name.clone(), password, remember, sound: s.sound, quality: s.quality };
        let session = viewer::view(self.ep.clone(), peer, opts, self.out.clone());
        self.tab = Tab::View(session.id);
        self.views.push(Viewing {
            session,
            peer,
            who: None,
            status: t("Connecting…"),
            link: None,
            control: Control::Off,
            tex: None,
            fps: 0,
            mods: Modifiers::NONE,
            scroll: Vec2::ZERO,
            captured: None,
            motion: Vec2::ZERO,
            monitors: Vec::new(),
            current: 0,
            privacy: false,
            sound: s.sound,
            quality: s.quality,
            actual_size: false,
            fullscreen: false,
            panel: Panel::None,
            chat: Chat::default(),
            listing: None,
            find: String::new(),
            transfers: Vec::new(),
            tools: false,
            their_files: None,
            notes: Vec::new(),
            password: String::new(),
            remember: true,
        });
        (self.need_password, self.note) = (false, String::new());
        self.password.clear();
    }

    /// The top bar: the mark and the name, a tab per screen being viewed, Settings at the end.
    /// The current tab is ink with a hairline under it; the others are soft.
    fn tabs(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        let before = self.tab;
        let frame = egui::Frame::new().fill(GROUND).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::top("tabs").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Pointer only: the Deskline tab beside it is the keyboard's way home.
                if look::mark(ui, &self.mark, 32.0).interact(Sense::CLICK).clicked() {
                    self.tab = Tab::Home;
                }
                ui.add_space(4.0);
                tab(ui, &mut self.tab, Tab::Home, "Deskline".into());
                for v in &self.views {
                    ui.add_space(8.0);
                    let mut label = v.who.clone().unwrap_or_else(|| t("Connecting…"));
                    if v.chat.unread > 0 {
                        label.push_str(&format!("  {}", v.chat.unread));
                    }
                    let name = format!("{} {}", t("Disconnect"), v.who.as_deref().unwrap_or_default());
                    tab(ui, &mut self.tab, Tab::View(v.session.id), label);
                    if look::close(ui, name.trim_end()).on_hover_text(t("Disconnect")).clicked() {
                        close = Some(v.session.id);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    tab(ui, &mut self.tab, Tab::Settings, t("Settings"));
                });
            });
        });
        if let Some(id) = close {
            self.hang_up(ui.ctx(), id);
        }
        // A note, and an undo, belong to the page they were written on.
        if self.tab != before {
            self.note.clear();
            self.forgotten = None;
        }
    }

    fn hang_up(&mut self, ctx: &egui::Context, id: u64) {
        self.views.retain(|v| v.session.id != id);
        if self.tab == Tab::View(id) {
            self.tab = Tab::Home;
        }
        if self.views.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
        }
    }

    /// Home, one column of type, nothing boxed: this computer's code for the person being
    /// helped, then the field for the helper, then the computers you've reached before.
    fn home(&mut self, ui: &mut egui::Ui) {
        column(ui, "home", |ui| {
            ui.add_space(40.0);
            ui.label(strong(t("Your code")));
            // The nine digits unless they couldn't be published (offline, say): then the long
            // code, which also works on the same network with no internet.
            let short = self.short.worked.load(Ordering::Relaxed);
            let code = if short { self.short.shown.clone() } else { self.code.clone() };
            let why = if short { "Read it out or send it to whoever is helping you." } else { "Send it to whoever is helping you. It never changes." };
            ui.label(look::prose(t(why)));
            ui.add_space(8.0);
            look::code(ui, &code);
            ui.add_space(8.0);
            // Wraps rather than push Change out of the window when the words run long.
            ui.horizontal_wrapped(|ui| {
                let now = ui.input(|i| i.time);
                let copied = self.copied_at.is_some_and(|at| now - at < 2.0);
                if copied {
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                }
                if primary(ui, if copied { t("Copied") } else { t("Copy code") }).clicked() {
                    ui.ctx().copy_text(code.replace(' ', ""));
                    self.copied_at = Some(now);
                }
                ui.add_space(8.0);
                let state = if self.password_on { t("Unattended access is on") } else { t("Unattended access is off") };
                ui.add(egui::Label::new(muted(state)).extend());
                if quiet(ui, t("Change")).clicked() {
                    self.tab = Tab::Settings;
                    self.note.clear();
                }
            });
            #[cfg(target_os = "macos")]
            self.mac_access(ui);

            ui.add_space(40.0);
            let heading = ui.label(strong(t("Connect to a computer")));
            ui.add_space(4.0);
            let mut go = false;
            ui.horizontal(|ui| {
                let ready = !self.partner.trim().is_empty();
                let label = t("Connect");
                let field = TextEdit::singleline(&mut self.partner).hint_text(t("Type or paste their code")).desired_width(look::beside(ui, &label));
                let field = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)).labelled_by(heading.id);
                let clicked = primary_enabled(ui, ready, label).clicked();
                go = (clicked || (field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && ready;
            });
            if self.need_password {
                let label = ui.label(soft(t("Their unattended-access password")));
                let pw = TextEdit::singleline(&mut self.password).password(true);
                let pw = ui.add(pw.desired_width(f32::INFINITY).min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)).labelled_by(label.id);
                go |= pw.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.partner.trim().is_empty();
                ui.checkbox(&mut self.remember, t("Log in without the password next time"));
            }
            if go {
                let password = (self.need_password && !self.password.is_empty()).then(|| self.password.clone());
                let (code, remember) = (self.partner.clone(), self.remember);
                self.connect(&code, password, remember);
            }
            if !self.note.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(soft(&self.note));
                    // An error: Settings' Report a problem, with the error already in it.
                    if self.trouble.as_ref() == Some(&self.note) && quiet(ui, t("Report this")).clicked() {
                        self.report.text = format!("{}\n\n", self.note);
                        self.report.focus = true;
                        self.tab = Tab::Settings;
                    }
                });
            }
            self.update_row(ui);
            self.contacts(ui);
            ui.add_space(40.0);
        });
    }

    /// macOS: Screen Recording and Accessibility as this Mac answers now, read again once a second
    /// at most (System Settings is where the answer changes).
    #[cfg(target_os = "macos")]
    fn mac_check(&mut self, now: f64) {
        if self.mac.checked.is_none_or(|at| now - at >= 1.0) {
            self.mac.granted = Access::ALL.map(Access::granted);
            self.mac.checked = Some(now);
        }
    }

    /// macOS: the two things this Mac has to allow before anyone can see or use it, asked here,
    /// where the person being helped already is, rather than while someone waits to get in. Gone
    /// once both are allowed.
    #[cfg(target_os = "macos")]
    fn mac_access(&mut self, ui: &mut egui::Ui) {
        self.mac_check(ui.input(|i| i.time));
        if self.mac.granted.iter().all(|g| *g) {
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        ui.add_space(24.0);
        ui.label(strong(t("Two things to allow on this Mac")));
        ui.label(look::prose(t("macOS asks once. Without them, whoever you let in can't see your screen or use your mouse.")));
        ui.add_space(4.0);
        // One ink button: the next thing to allow.
        let next = self.mac.granted.iter().position(|g| !g);
        for (i, a) in Access::ALL.into_iter().enumerate() {
            let (name, why) = match a {
                Access::Screen => (t("Screen Recording"), t("So they can see your screen")),
                Access::Control => (t("Accessibility"), t("So they can use your mouse and keyboard")),
            };
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(strong(name));
                    ui.label(muted(why));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.mac.granted[i] {
                        ui.label(muted(t("Allowed")));
                        return;
                    }
                    let label = if self.mac.asked[i] { t("Open System Settings") } else { t("Allow") };
                    let r = if next == Some(i) { primary(ui, label) } else { ui.button(label) };
                    if r.clicked() {
                        a.ask(!self.mac.asked[i]);
                        self.mac.asked[i] = true;
                    }
                });
            });
        }
        // Screen Recording reaches a running app only once it restarts.
        if self.mac.asked[0] && !self.mac.granted[0] {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(soft(t("Switched Screen Recording on and it still isn't allowed? macOS shows Deskline the change once it restarts.")));
                if quiet(ui, t("Restart Deskline")).clicked() {
                    update::relaunch();
                    self.quitting = true;
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
        }
    }

    fn update_row(&mut self, ui: &mut egui::Ui) {
        let Some(u) = self.updater.as_mut() else { return };
        let Some((tag, _)) = u.ready.clone() else { return };
        ui.add_space(16.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(soft(tf("Deskline {} is ready.", &[&tag])));
            let label = if u.installing { t("Updating…") } else { t("Restart to update") };
            if primary_enabled(ui, !u.installing, label).clicked() {
                u.install();
            }
        });
    }

    /// The computers you've reached before. Connect, or click anywhere on the row; Forget can be
    /// undone until you leave the page.
    fn contacts(&mut self, ui: &mut egui::Ui) {
        let contacts = self.store.get().contacts;
        if contacts.is_empty() && self.forgotten.is_none() {
            return;
        }
        ui.add_space(40.0);
        ui.label(strong(t("Recent")));
        ui.add_space(4.0);
        let mut connect = None;
        for (i, c) in contacts.iter().enumerate() {
            let renaming = self.renaming.as_ref().is_some_and(|(id, _)| *id == c.id);
            // The row takes the pointer; Connect on it is the keyboard's way in.
            let sense = if renaming { Sense::hover() } else { Sense::CLICK };
            let (row, _) = look::row(ui, sense, |ui| {
                ui.horizontal(|ui| {
                    match self.renaming.as_mut().filter(|(id, _)| *id == c.id) {
                        Some((_, draft)) => {
                            let r = ui.add(TextEdit::singleline(draft).desired_width(200.0)).accessible_name(t("Rename"));
                            r.request_focus();
                            if r.lost_focus() || quiet(ui, t("Save")).clicked() {
                                let alias = Some(draft.trim().to_string()).filter(|a| !a.is_empty());
                                let id = c.id.clone();
                                let _ = self.store.update(|s| {
                                    if let Some(x) = s.contacts.iter_mut().find(|x| x.id == id) {
                                        x.alias = alias;
                                    }
                                });
                                self.renaming = None;
                            }
                        }
                        None => {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(strong(c.label()));
                                ui.label(muted(format!("{}  ·  {}", &c.id[..8], settings::when(c.last))));
                            });
                        }
                    }
                    // Every row has the same three words; a screen reader also hears whose row.
                    let whose = |word: String| format!("{word} {}", c.label());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if quiet(ui, t("Forget")).accessible_name(whose(t("Forget"))).clicked() {
                            let id = c.id.clone();
                            let _ = self.store.update(|s| s.contacts.retain(|x| x.id != id));
                            self.forgotten = Some((i, c.clone()));
                        }
                        if !renaming && quiet(ui, t("Rename")).accessible_name(whose(t("Rename"))).clicked() {
                            self.renaming = Some((c.id.clone(), c.alias.clone().unwrap_or_default()));
                        }
                        // Ink words, no frame until the pointer is on it: the row's action.
                        let connect_button = egui::Button::new(t("Connect")).frame_when_inactive(false);
                        if !renaming && ui.add(connect_button).accessible_name(whose(t("Connect"))).clicked() {
                            connect = Some(c.id.clone());
                        }
                    });
                })
            });
            if row.clicked() {
                connect = Some(c.id.clone());
            }
        }
        if let Some((i, c)) = self.forgotten.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(soft(tf("{} is no longer in Recent.", &[c.label()])));
                if quiet(ui, t("Undo")).clicked() {
                    let _ = self.store.update(|s| {
                        if !s.contacts.iter().any(|x| x.id == c.id) {
                            s.contacts.insert(i.min(s.contacts.len()), c);
                        }
                    });
                    self.forgotten = None;
                }
            });
        }
        if let Some(id) = connect {
            self.partner = id.clone();
            self.connect(&id, None, false);
        }
    }

    #[cfg(windows)]
    fn install(&mut self) {
        // UAC waits for the user: off the UI thread.
        std::thread::spawn(|| match service::elevate(&["--install"]) {
            Ok(()) => {
                // The installed copy talks to the service; this portable one steps aside.
                if let Some(pf) = std::env::var_os("ProgramFiles") {
                    let _ = std::process::Command::new(PathBuf::from(pf).join("Deskline").join("deskline.exe")).spawn();
                }
                std::process::exit(0);
            }
            Err(e) => eprintln!("deskline: install: {e}"),
        });
        self.note = t("Installing… confirm the Windows prompt.");
    }

    /// Settings, the same column: sections divided by hairlines, never boxed.
    fn settings(&mut self, ui: &mut egui::Ui) {
        column(ui, "settings", |ui| {
            ui.add_space(40.0);
            self.security(ui);
            ui.add_space(24.0);
            look::hairline(ui);
            ui.add_space(16.0);
            self.preferences(ui);
            ui.add_space(24.0);
            look::hairline(ui);
            ui.add_space(16.0);
            self.system(ui);
            ui.add_space(24.0);
            look::hairline(ui);
            ui.add_space(16.0);
            self.report(ui);
            if !self.note.is_empty() {
                ui.add_space(8.0);
                ui.label(soft(&self.note));
            }
            ui.add_space(32.0);
            ui.label(muted(format!("Deskline {}", env!("CARGO_PKG_VERSION"))));
            ui.add_space(24.0);
        });
    }

    /// A problem in the person's words, with what Deskline knows about this computer, opened as an
    /// issue on Deskline's GitHub or copied to send any other way. Never the code or who you've
    /// reached: the line under the field says exactly what goes with it.
    fn report(&mut self, ui: &mut egui::Ui) {
        ui.label(strong(t("Report a problem")));
        ui.label(look::prose(t(
            "Say what happened. Open on GitHub starts an issue there, which needs a GitHub account. Copy puts it on your clipboard to send any other way.",
        )));
        ui.add_space(4.0);
        let label = ui.label(soft(t("What happened?")));
        let field = ui.add(TextEdit::multiline(&mut self.report.text).desired_rows(4).desired_width(f32::INFINITY)).labelled_by(label.id);
        if std::mem::take(&mut self.report.focus) {
            field.request_focus();
            field.scroll_to_me(Some(Align::Center));
        }
        let about = self.about(ui.input(|i| i.time));
        ui.label(muted(tf("Sent with it: {}. Never your code or the computers you've reached.", &[&about.replace('\n', ", ")])));
        ui.add_space(4.0);
        let story = self.report.text.trim().to_string();
        ui.horizontal_wrapped(|ui| {
            if primary_enabled(ui, !story.is_empty(), t("Open on GitHub")).clicked() {
                open(std::path::Path::new(&issue_url(&story, &about)));
                self.note = t("Opened in your browser: check it there, then click Create.");
            }
            let now = ui.input(|i| i.time);
            let copied = self.report.copied_at.is_some_and(|at| now - at < 2.0);
            if copied {
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
            let label = if copied { t("Copied") } else { t("Copy") };
            if ui.add_enabled(!story.is_empty(), egui::Button::new(label)).clicked() {
                ui.ctx().copy_text(format!("{story}\n\n{about}"));
                self.report.copied_at = Some(now);
            }
        });
    }

    /// What a report says about this computer, in English (it's read by whoever fixes it), one
    /// thing a line.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn about(&mut self, now: f64) -> String {
        let mut lines = vec![format!("Deskline {}", env!("CARGO_PKG_VERSION")), system_name().to_string()];
        if cfg!(windows) {
            lines.push(if self.service { "Installed for all users" } else { "Portable" }.into());
        }
        #[cfg(target_os = "macos")]
        {
            self.mac_check(now);
            for (name, g) in ["Screen Recording", "Accessibility"].into_iter().zip(self.mac.granted) {
                lines.push(format!("{name}: {}", if g { "allowed" } else { "not allowed" }));
            }
        }
        lines.join("\n")
    }

    /// Who gets in without a click. With the service, only an elevated window may change it.
    fn security(&mut self, ui: &mut egui::Ui) {
        ui.label(strong(t("Unattended access")));
        ui.label(look::prose(t(
            "With a password, people who know it can connect and control this computer when nobody is here to click Accept.",
        )));
        // Home's nine digits are new every run: unattended access needs the code that stays.
        ui.add_space(8.0);
        ui.label(soft(t("This computer's permanent code, for connecting with the password")));
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&self.code).monospace().color(INK));
            if quiet(ui, t("Copy code")).clicked() {
                ui.ctx().copy_text(self.code.replace(' ', ""));
            }
        });
        ui.add_space(8.0);
        #[cfg(windows)]
        let locked = self.service && !service::is_elevated();
        #[cfg(not(windows))]
        let locked = false;
        if locked {
            ui.label(muted(if self.password_on { t("A password is set.") } else { t("No password is set.") }));
            if ui.button(t("Change as administrator")).clicked() {
                #[cfg(windows)]
                std::thread::spawn(|| service::elevate(&["--settings"]));
            }
        } else {
            ui.label(muted(if self.password_on { t("A password is set. Set a new one to replace it.") } else { t("No password is set.") }));
            // Labels stay above the fields: a hint would vanish at the first letter.
            ui.add_space(4.0);
            let label = ui.label(soft(t("New password (8 characters or more)")));
            ui.add(secret(&mut self.form.pw)).labelled_by(label.id);
            let label = ui.label(soft(t("Repeat it")));
            ui.add(secret(&mut self.form.pw2)).labelled_by(label.id);
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if primary(ui, t("Set password")).clicked() {
                    if self.form.pw.chars().count() < 8 {
                        self.form.error = t("Use at least 8 characters.");
                    } else if self.form.pw != self.form.pw2 {
                        self.form.error = t("The two passwords are different.");
                    } else {
                        let _ = self.host.send(HostCmd::SetPassword(Some(std::mem::take(&mut self.form.pw))));
                        self.form.pw2.clear();
                        self.form.error = t("Password set.");
                    }
                }
                if self.password_on && quiet(ui, t("Turn off")).clicked() {
                    let _ = self.host.send(HostCmd::SetPassword(None));
                    self.form.error = t("Unattended access is off.");
                }
            });
            if !self.form.error.is_empty() {
                ui.label(soft(&self.form.error));
            }
        }
        if !self.trusted.is_empty() {
            ui.add_space(16.0);
            ui.label(strong(t("Always allowed")));
            for p in self.trusted.clone() {
                look::row(ui, Sense::hover(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&p.name).color(INK));
                        ui.label(muted(&p.id[..8]).monospace());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add_enabled(!locked, egui::Button::new(soft(t("Remove"))).frame_when_inactive(false)).clicked() {
                                let _ = self.host.send(HostCmd::Untrust(p.id.clone()));
                            }
                        });
                    });
                });
            }
        }
    }

    fn preferences(&mut self, ui: &mut egui::Ui) {
        let mut s = self.store.get();
        let before = s.clone();
        ui.label(strong(t("Sessions")));
        ui.add_space(4.0);
        ui.checkbox(&mut s.allow_control, t("When I accept someone, let them control by default"));
        ui.checkbox(&mut s.sound, t("Hear the other computer's sound"));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(soft(t("Picture")));
            ui.add_space(8.0);
            for q in QUALITIES {
                ui.selectable_value(&mut s.quality, q, quality_name(q));
            }
        });
        if s != before {
            let _ = self.store.update(|x| *x = s);
        }
    }

    fn system(&mut self, ui: &mut egui::Ui) {
        ui.label(strong(t("This computer")));
        ui.add_space(4.0);
        if !self.service {
            let mut on = autostart::enabled();
            if ui.checkbox(&mut on, t("Start Deskline when I sign in")).changed() {
                if let Err(e) = autostart::set(on) {
                    self.note = tf("Couldn't change that: {}", &[&e.to_string()]);
                }
            }
        }
        #[cfg(windows)]
        if self.service {
            ui.label(look::prose(t("Installed: Deskline starts with Windows and works at the sign-in screen.")));
            if quiet(ui, t("Uninstall")).clicked() {
                std::thread::spawn(|| {
                    if service::elevate(&["--uninstall"]).is_ok() {
                        std::process::exit(0);
                    }
                });
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                if ui.button(t("Install for all users")).clicked() {
                    self.install();
                }
                ui.label(muted(t("Starts with Windows and works at the sign-in screen.")));
            });
        }
        ui.add_space(16.0);
        let heading = ui.label(strong(t("Relay server")));
        ui.label(look::prose(t(
            "Leave empty for the public relays. Your own: run iroh-relay and paste its URL. Restart Deskline after changing.",
        )));
        ui.horizontal(|ui| {
            let label = t("Save");
            let field = TextEdit::singleline(&mut self.form.relay).hint_text("https://relay.example.com").desired_width(look::beside(ui, &label));
            ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)).labelled_by(heading.id);
            if ui.button(label).clicked() {
                let relay = Some(self.form.relay.trim().to_string()).filter(|r| !r.is_empty());
                let _ = self.store.update(|s| s.relay = relay);
                self.note = t("Saved. Restart Deskline to use it.");
            }
        });
        ui.add_space(16.0);
        if self.service {
            ui.label(muted(t("Session log: %ProgramData%\\Deskline\\sessions.log (administrators)")));
        } else if quiet(ui, t("Open session log")).clicked() {
            open(&self.store.dir().join("sessions.log"));
        }
    }

    /// Waiting on the other computer: whose code, what's happening, and the line between the
    /// two computers being drawn in dusk while it happens.
    fn connecting(&mut self, ui: &mut egui::Ui, i: usize) {
        let mut cancel = false;
        let mut login = None;
        let v = &mut self.views[i];
        let top = (ui.available_height() * 0.22).max(40.0);
        // Scrolls when the password form doesn't fit a short window, so Cancel is never cut off.
        ScrollArea::vertical().id_salt(("connecting", v.session.id)).auto_shrink(false).show(ui, |ui| ui.vertical_centered(|ui| {
            ui.set_max_width(360.0);
            ui.add_space(top);
            ui.label(strong(&v.status));
            ui.label(muted(&net::code(v.peer)[..19]).monospace());
            ui.add_space(16.0);
            look::waiting(ui, 280.0);
            ui.add_space(24.0);
            if v.status == t("Waiting for them to accept…") {
                look::hairline(ui);
                ui.add_space(16.0);
                let label = ui.label(soft(t("Or log in with their unattended-access password")));
                let field = TextEdit::singleline(&mut v.password).password(true).desired_width(f32::INFINITY);
                let r = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)).labelled_by(label.id);
                ui.checkbox(&mut v.remember, t("Log in without the password next time"));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (primary_enabled(ui, !v.password.is_empty(), t("Log in")).clicked() || enter) && !v.password.is_empty() {
                    login = Some((net::code(v.peer), v.password.clone(), v.remember));
                }
                ui.add_space(8.0);
            }
            cancel = quiet(ui, t("Cancel")).clicked();
            ui.add_space(24.0);
        }));
        if cancel || login.is_some() {
            let id = self.views[i].session.id;
            self.hang_up(ui.ctx(), id);
        }
        if let Some((code, pw, remember)) = login {
            self.connect(&code, Some(pw), remember);
        }
    }

    fn viewer(&mut self, ui: &mut egui::Ui, i: usize) {
        let mut hang_up = false;
        let picked = self.picked.0.clone();
        let v = &mut self.views[i];
        let control = v.control == Control::On;

        if !v.fullscreen {
            toolbar(ui, v, &mut hang_up);
        }
        if v.fullscreen && ui.input(|i| i.key_pressed(Key::F11)) {
            v.fullscreen = false;
            ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(false));
        }

        let side = egui::Frame::new().fill(GROUND).inner_margin(egui::Margin::same(16)).stroke(egui::Stroke::new(1.0, HAIRLINE));
        match v.panel {
            Panel::Files => {
                egui::Panel::right("files").frame(side).default_size(360.0).show(ui, |ui| files_panel(ui, v, control, &picked));
            }
            Panel::Chat => {
                v.chat.unread = 0;
                egui::Panel::right("chat").frame(side).default_size(300.0).show(ui, |ui| {
                    if let Some(text) = chat_panel(ui, &mut v.chat) {
                        v.session.send(Ask::Chat(text));
                    }
                });
            }
            Panel::None => {}
        }

        // Files dropped on the window go to the folder on show in Files, else their Downloads.
        let (to, dropping) = (v.send_to(), ui.input(|i| !i.raw.hovered_files.is_empty()));
        if control {
            for f in ui.input(|i| i.raw.dropped_files.clone()) {
                v.session.cmd(Cmd::Upload(f.path().to_path_buf(), to.clone()));
            }
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            if let Some(img) = v.session.frame.lock().unwrap().take() {
                match v.tex.as_mut() {
                    Some(tex) => tex.set(img, TextureOptions::LINEAR),
                    None => v.tex = Some(ui.ctx().load_texture("screen", img, TextureOptions::LINEAR)),
                }
            }
            let area = ui.available_rect_before_wrap();
            let Some(tex) = v.tex.clone() else {
                ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, t("Waiting for the first picture…"), egui::FontId::proportional(look::SIZE), SOFT);
                return;
            };
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            if v.actual_size {
                // One screen pixel per display pixel.
                let size = tex.size_vec2() / ui.ctx().pixels_per_point();
                let source = if control { ScrollSource::SCROLL_BAR } else { ScrollSource::ALL };
                ScrollArea::both().scroll_source(source).show(ui, |ui| {
                    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
                    ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                    if control {
                        forward_input(ui, v, rect, response.hovered());
                    }
                });
            } else {
                let response = ui.allocate_rect(area, Sense::hover());
                let size = tex.size_vec2();
                let scale = (area.width() / size.x).min(area.height() / size.y);
                let rect = Rect::from_center_size(area.center(), size * scale);
                // On whole pixels: a picture shown 1:1 but half a pixel off is all blurred.
                let rect = Rect::from_min_size(rect.min.round_to_pixels(ui.ctx().pixels_per_point()), rect.size());
                ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                if control {
                    forward_input(ui, v, rect, response.hovered());
                }
            }
            let pasting = v.transfers.iter().find(|x| x.end.is_none() && matches!(x.what, What::PasteThere(_) | What::PasteHere(_)));
            if !v.status.is_empty() {
                // Reconnecting: say so over the frozen picture.
                plate(ui, area, v.status.clone());
            } else if dropping && control {
                let folder = to.as_deref().map(|p| trail(p).pop().map_or_else(|| p.to_string(), |(name, _)| name));
                plate(ui, area, folder.map_or_else(|| t("Drop to send to their Downloads"), |f| tf("Drop to send to {}", &[&f])));
            } else if let Some(x) = pasting {
                // A paste waits on its files: say so where the eyes are.
                plate(ui, area, format!("{}  {}", what(&x.what), percent(x)));
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            } else if let Some(at) = v.captured {
                // The way out of a captured mouse, for its first seconds.
                let left = 3.0 - (ui.input(|i| i.time) - at);
                if left > 0.0 {
                    plate(ui, area, t("Ctrl+Alt+Shift+Z gives the mouse back"));
                    ui.ctx().request_repaint_after(Duration::from_secs_f64(left));
                }
            }
        });

        if hang_up {
            let id = self.views[i].session.id;
            self.hang_up(ui.ctx(), id);
        }
    }

    /// While someone is in this computer: a red bar per person at the top of the window, with
    /// the way to stop them one click away (and `float_bar` when the window isn't in front).
    fn banners(&mut self, ui: &mut egui::Ui) {
        let mut cmds = Vec::new();
        for s in &mut self.shares {
            let frame = egui::Frame::new().fill(RED).inner_margin(egui::Margin::symmetric(16, 8));
            egui::Panel::top(Id::unique(("sharing", s.id))).frame(frame).show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Buttons first, from the end; the sentence takes what's left and wraps,
                    // so in a narrow window or a long language nothing runs under the buttons.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if look::stop(ui, t("Stop sharing")).clicked() {
                            cmds.push(HostCmd::Stop { id: s.id });
                        }
                        if s.control && look::on_red(ui, t("Take back control")).clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: false });
                        }
                        if !s.control && !s.asking && look::on_red(ui, t("Give control")).clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: true });
                        }
                        // The portal's answer (which screens, remote interaction) can change.
                        if cfg!(target_os = "linux")
                            && look::on_red(ui, t("Choose screens")).on_hover_text(t("Pick again what they see, and whether they may control")).clicked()
                        {
                            cmds.push(HostCmd::Reshare);
                        }
                        let chat = if s.chat.unread > 0 { format!("{}  {}", t("Chat"), s.chat.unread) } else { t("Chat") };
                        if look::on_red(ui, chat).clicked() {
                            s.chat.open = !s.chat.open;
                            s.chat.unread = 0;
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let what = if s.control { "{} sees and controls your screen" } else { "{} sees your screen" };
                            ui.add(egui::Label::new(strong(tf(what, &[&s.who])).color(Color32::WHITE)).wrap());
                        });
                    });
                });
            });
            if s.chat.open {
                let mut open = true;
                egui::Window::new(tf("Chat with {}", &[&s.who]))
                    .id(Id::unique(("chat", s.id)))
                    .open(&mut open)
                    .default_size([320.0, 320.0])
                    .show(ui.ctx(), |ui| {
                        if let Some(text) = chat_panel(ui, &mut s.chat) {
                            cmds.push(HostCmd::Chat { id: s.id, text });
                        }
                    });
                s.chat.open = open;
            }
        }
        for c in cmds {
            if let HostCmd::AllowControl { id, yes: false } = c {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    s.control = false;
                }
            }
            let _ = self.host.send(c);
        }
    }

    /// While someone is in this computer and the window isn't in front (behind other windows, or
    /// minimized), the red bar also stands alone above every other window. Called from `logic`,
    /// which runs while the window is minimized; `ui` doesn't.
    fn float_bar(&mut self, ctx: &egui::Context) {
        let (focused, minimized) = ctx.input(|i| (i.focused, i.viewport().minimized == Some(true)));
        if self.shares.is_empty() || self.hidden || (focused && !minimized) {
            return;
        }
        let who = self.shares.iter().map(|s| s.who.as_str()).collect::<Vec<_>>().join(", ");
        let what = if self.shares.iter().any(|s| s.control) { "{} sees and controls your screen" } else { "{} sees your screen" };
        let line = tf(what, &[&who]);
        let monitor = ctx.input(|i| i.viewport().monitor_size).unwrap_or(vec2(1280.0, 800.0));
        let size = vec2(560.0, 48.0);
        let bar = egui::ViewportBuilder::default()
            .with_title("Deskline")
            .with_always_on_top()
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_inner_size(size)
            .with_position(Pos2::new(((monitor.x - size.x) / 2.0).max(0.0), 12.0));
        let stop = ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("sharing-bar"), bar, |ui, _| {
            let mut stop = false;
            let frame = egui::Frame::new().fill(RED).inner_margin(egui::Margin::symmetric(16, 8));
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                ui.horizontal(|ui| {
                    // The bar's height is fixed: a sentence too long for it ends in "…", whole on hover.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        stop = look::stop(ui, t("Stop sharing")).clicked();
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(strong(line.clone()).color(Color32::WHITE)).truncate());
                        });
                    });
                });
            });
            stop
        });
        if stop {
            for s in &self.shares {
                let _ = self.host.send(HostCmd::Stop { id: s.id });
            }
        }
    }

    /// The one question the person being helped has to answer, said plainly.
    fn prompts(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.requests.first_mut() {
            let id = Id::unique(("request", r.req));
            let answer = ask(ctx, id, 380.0, t("Someone wants to see your screen"), [t("Accept"), t("Decline")], |ui| {
                ui.label(RichText::new(&r.who).size(20.0).color(INK));
                ui.label(look::prose(t("Only accept if you know who this is.")));
                ui.add_space(16.0);
                ui.checkbox(&mut r.control, t("Let them control (mouse, keyboard, clipboard, files)"));
                ui.checkbox(&mut r.trust, t("Always allow them, even when I'm away"));
            });
            if let Some(yes) = answer {
                let r = self.requests.remove(0);
                // Accessibility is asked now, while they're at the Mac, rather than at the first
                // click from the other side.
                #[cfg(target_os = "macos")]
                if yes && r.control && !Access::Control.granted() {
                    Access::Control.ask(!self.mac.asked[1]);
                    self.mac.asked[1] = true;
                }
                let _ = self.host.send(HostCmd::Answer { req: r.req, yes, control: r.control && yes, trust: r.trust && yes });
            }
        }

        // One question at a time: control first.
        let asking = self.shares.iter().any(|s| s.asking);
        if let Some(s) = self.shares.iter_mut().find(|s| s.screens).filter(|_| !asking) {
            let answer = ask(ctx, Id::unique(("screens", s.id)), 360.0, t("Other screens"), [t("Choose screens"), t("Not now")], |ui| {
                ui.label(RichText::new(tf("{} asks you to choose again what you share.", &[&s.who])).color(INK));
                ui.label(look::prose(t("Your system's sharing window opens: pick the screens they see, and whether they may control.")));
            });
            if let Some(yes) = answer {
                s.screens = false;
                let _ = self.host.send(HostCmd::Screens { id: s.id, yes });
            }
        }

        let Some(s) = self.shares.iter_mut().find(|s| s.asking) else { return };
        let answer = ask(ctx, Id::unique(("control", s.id)), 360.0, t("Control request"), [t("Allow"), t("Deny")], |ui| {
            ui.label(RichText::new(tf("{} wants to control your mouse and keyboard.", &[&s.who])).color(INK));
            ui.label(look::prose(t("You can take control back at any time from the red bar.")));
        });
        if let Some(yes) = answer {
            s.asking = false;
            let _ = self.host.send(HostCmd::AllowControl { id: s.id, yes });
        }
    }

    /// Short codes: this computer's points at its long code while the window is open (hidden in
    /// the tray, it stops being renewed and soon finds nobody); theirs, once looked up, connects.
    fn short_code(&mut self, ctx: &egui::Context) {
        let target = net::parse_code(&self.code).filter(|_| !self.hidden);
        if target != self.short.sent {
            self.short.sent = target;
            let _ = self.short.target.send(target);
        }
        let Some(lookup) = &self.lookup else { return };
        let answer = match lookup.answer.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return ctx.request_repaint_after(Duration::from_millis(100)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("the lookup stopped")),
        };
        let Lookup { password, remember, .. } = self.lookup.take().expect("checked above");
        match answer {
            Ok(Some(peer)) => self.connect(&net::code(peer), password, remember),
            Ok(None) => self.note = t("No computer is showing that code. Check the digits, and that Deskline is open on their computer."),
            Err(e) => self.note = tf("Couldn't look up that code: {}", &[&format!("{e:#}")]),
        }
    }

    /// Tray clicks, updates, closing to the tray.
    fn housekeeping(&mut self, ctx: &egui::Context) {
        let mut msgs = Vec::new();
        if let Some(tray) = &self.tray {
            while let Ok(m) = tray.rx.try_recv() {
                msgs.push(m);
            }
        }
        for m in msgs {
            match m {
                TrayMsg::Open => self.show(ctx),
                TrayMsg::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        if let Some(tray) = self.tray.as_mut() {
            tray.alert(!self.requests.is_empty() || !self.shares.is_empty());
            tray.tooltip(&match self.shares.first() {
                None => "Deskline".to_string(),
                Some(s) => tf(if s.control { "{} sees and controls your screen" } else { "{} sees your screen" }, &[&s.who]),
            });
        }
        if let Some(u) = self.updater.as_mut() {
            while let Ok(m) = u.rx.try_recv() {
                match m {
                    update::Msg::Ready(tag, file) => u.ready = Some((tag, file)),
                    update::Msg::Installed => {
                        update::relaunch();
                        self.quitting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                    update::Msg::Failed(e) => {
                        u.installing = false;
                        self.note = tf("Update failed: {}", &[&e]);
                        self.trouble = Some(self.note.clone());
                    }
                }
            }
        }
        // Closing keeps this computer reachable from the tray, unless the service does that, and
        // never hides the red bar while someone is in.
        let idle = self.views.is_empty() && self.shares.is_empty();
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.tray.is_some() && idle {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.hidden = true;
        }
    }
}

impl Viewing {
    fn note(&mut self, text: &str) {
        self.notes.push(text.to_string());
        if self.notes.len() > 3 {
            self.notes.remove(0);
        }
    }

    /// Where files sent now land: the folder open in Files, when their computer takes that,
    /// else their Downloads (None).
    fn send_to(&self) -> Option<String> {
        let l = self.listing.as_ref().filter(|_| self.tools && self.panel == Panel::Files)?;
        (!l.path.is_empty()).then(|| l.path.clone())
    }
}

/// The session's bar: who, the link drawn as dusk with its kind and round trip, control, the
/// session's tools, Disconnect at the end. A hairline under it. The tools are words with no
/// frame until the pointer is on them (or they're on); only Ask for control is filled.
fn toolbar(ui: &mut egui::Ui, v: &mut Viewing, hang_up: &mut bool) {
    let frame = egui::Frame::new().fill(GROUND).inner_margin(egui::Margin::symmetric(16, 8)).stroke(egui::Stroke::new(1.0, HAIRLINE));
    egui::Panel::top("viewer").frame(frame).show(ui, |ui| {
        let rest = &mut ui.visuals_mut().widgets.inactive;
        (rest.bg_fill, rest.weak_bg_fill, rest.bg_stroke) = (Color32::TRANSPARENT, Color32::TRANSPARENT, egui::Stroke::NONE);
        ui.horizontal_wrapped(|ui| {
            ui.label(strong(v.who.as_deref().unwrap_or("…")));
            let (quality, text) = match v.link {
                Some((kind, ms)) if v.status.is_empty() => (Some(look::link_quality(kind, ms)), format!("{}  {ms} ms", t(kind))),
                _ => (None, v.status.clone()),
            };
            look::link(ui, quality).on_hover_text(&text);
            match v.link {
                // The round trip in mono: a measurement, and its digits don't shift the bar as it changes.
                Some((kind, ms)) if v.status.is_empty() => {
                    ui.label(muted(t(kind)));
                    ui.label(muted(format!("{ms} ms")).monospace());
                    if v.fps > 0 {
                        ui.label(muted(format!("{} fps", v.fps)).monospace());
                    }
                }
                _ => {
                    ui.label(muted(text));
                }
            }
            ui.add_space(8.0);
            match v.control {
                Control::Off => {
                    if primary(ui, t("Ask for control")).clicked() {
                        v.session.send(Ask::WantControl);
                        v.control = Control::Asked;
                    }
                }
                Control::Asked => {
                    ui.label(soft(t("Waiting for them to allow control…")));
                }
                Control::On => {
                    ui.label(RichText::new(t("You have control")).color(GREEN));
                    if ui.button("Ctrl+Alt+Del").clicked() {
                        v.session.send(Ask::Input(Input::CtrlAltDel));
                    }
                    if ui.selectable_label(v.privacy, t("Privacy")).on_hover_text(t("Blank their screen and lock their own mouse and keyboard")).clicked() {
                        v.session.send(Ask::Privacy(!v.privacy));
                    }
                    let hint = t("For games: your mouse moves theirs directly. Ctrl+Alt+Shift+Z gives it back.");
                    if ui.selectable_label(v.captured.is_some(), t("Capture mouse")).on_hover_text(hint).clicked() {
                        v.captured = if v.captured.is_some() { None } else { Some(ui.input(|i| i.time)) };
                    }
                }
            }
            // Always there, even with one screen: the system may share fewer than they have.
            if !v.monitors.is_empty() {
                let current = v.monitors.get(v.current).map_or(t("Screen"), |m| m.name.clone());
                let mut pick = v.current;
                let mut other = false;
                egui::ComboBox::from_id_salt("monitor").selected_text(current).show_ui(ui, |ui| {
                    for (n, m) in v.monitors.iter().enumerate() {
                        ui.selectable_value(&mut pick, n, format!("{}  {}×{}", m.name, m.w, m.h));
                    }
                    other = ui.selectable_label(false, t("Other screens…")).on_hover_text(t("Asks their computer to choose again what it shares")).clicked();
                });
                if pick != v.current {
                    v.session.send(Ask::Monitor(pick));
                }
                if other {
                    v.session.send(Ask::Reshare);
                    v.note(&t("Asked them to choose again what they share."));
                }
            }
            let mut q = v.quality;
            egui::ComboBox::from_id_salt("quality").selected_text(quality_name(q)).show_ui(ui, |ui| {
                for x in QUALITIES {
                    ui.selectable_value(&mut q, x, quality_name(x));
                }
            });
            if q != v.quality {
                v.quality = q;
                v.session.send(Ask::Quality(q));
            }
            if ui.selectable_label(v.sound, t("Sound")).clicked() {
                v.sound = !v.sound;
                v.session.send(Ask::Sound(v.sound));
            }
            if ui.selectable_label(v.actual_size, t("Actual size")).clicked() {
                v.actual_size = !v.actual_size;
            }
            if ui.button(t("Full screen")).on_hover_text(t("F11 to leave")).clicked() {
                v.fullscreen = true;
                ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(true));
            }
            if ui.selectable_label(v.panel == Panel::Files, t("Files")).clicked() {
                v.panel = if v.panel == Panel::Files { Panel::None } else { Panel::Files };
                if v.panel == Panel::Files && v.listing.is_none() {
                    v.session.cmd(Cmd::List(String::new()));
                }
            }
            let chat = if v.chat.unread > 0 { format!("{}  {}", t("Chat"), v.chat.unread) } else { t("Chat") };
            if ui.selectable_label(v.panel == Panel::Chat, chat).clicked() {
                v.panel = if v.panel == Panel::Chat { Panel::None } else { Panel::Chat };
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                *hang_up = quiet(ui, t("Disconnect")).clicked();
            });
        });
        for n in &v.notes {
            ui.label(muted(n));
        }
        if let Some((count, bytes)) = v.their_files {
            ui.horizontal_wrapped(|ui| {
                ui.label(muted(tf("They copied {} ({}).", &[&files_count(count), &size(bytes)])));
                if quiet(ui, t("Copy to my clipboard")).clicked() {
                    v.session.cmd(Cmd::CopyTheirs);
                    v.their_files = None;
                }
            });
        }
    });
}

/// A line over the picture near its top, on a dark plate so it reads on anything.
fn plate(ui: &egui::Ui, area: Rect, text: String) {
    let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(look::SIZE), INK);
    let plate = Rect::from_center_size(area.center_top() + vec2(0.0, 40.0), galley.size() + vec2(32.0, 16.0));
    ui.painter().rect_filled(plate, 10.0, Color32::from_black_alpha(200));
    ui.painter().galley(plate.center() - galley.size() / 2.0, galley, INK);
}

/// A tab: ink with a hairline under it when it's the current one, soft otherwise.
fn tab(ui: &mut egui::Ui, current: &mut Tab, this: Tab, label: String) {
    let on = *current == this;
    let text = if on { strong(label) } else { soft(label) };
    let r = ui.add(egui::Label::new(text).selectable(false).sense(Sense::click()));
    if on {
        let y = r.rect.bottom() + 6.0;
        ui.painter().hline(r.rect.x_range(), y, egui::Stroke::new(1.0, INK));
    } else if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if r.has_focus() {
        look::focus_ring(ui, r.rect.expand2(vec2(6.0, 4.0)), 6.0, INK, egui::StrokeKind::Outside);
    }
    if r.clicked() {
        *current = this;
    }
}

/// luannzin.com's reading column: centred, left-aligned inside, 16px from the window's edges at
/// the narrowest. The whole window scrolls it, margins included, with the bar at the window's
/// edge; each page (`id`) keeps its own place.
fn column(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    ScrollArea::vertical().id_salt(id).auto_shrink(false).show(ui, |ui| {
        let mut rect = ui.max_rect();
        let width = COLUMN.min(rect.width() - 32.0);
        rect.min.x += (rect.width() - width) / 2.0;
        rect.max.x = rect.min.x + width;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add);
    });
}

/// A password field at the interface's height.
fn secret(s: &mut String) -> TextEdit<'_> {
    TextEdit::singleline(s).password(true).min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)
}

/// A prompt's width, narrower when the window is (small, or zoomed in): never cut off.
fn prompt_width(ctx: &egui::Context, width: f32) -> f32 {
    width.min(ctx.content_rect().width() - 2.0 * (24.0 + 16.0)).max(160.0)
}

fn prompt_frame() -> egui::Frame {
    egui::Frame::new().fill(look::TILE).corner_radius(14).inner_margin(egui::Margin::same(24)).stroke(egui::Stroke::new(1.0, HAIRLINE))
}

/// One question for the person at this computer: `title`, what `body` adds, then the ink yes
/// and the plain no of `[yes, no]`. The prompt is a dialog named by its title. Escape says no,
/// and when the prompt first shows, keyboard focus (and a screen reader with it) lands on no:
/// the window jumps forward on its own, and a key meant for another window can only turn
/// someone away, never let them in.
fn ask(ctx: &egui::Context, id: Id, width: f32, title: String, [yes, no]: [String; 2], body: impl FnOnce(&mut egui::Ui)) -> Option<bool> {
    let mut answer = None;
    Modal::new(id).frame(prompt_frame()).show(ctx, |ui| {
        ui.set_width(prompt_width(ctx, width));
        if let Some(dialog) = ui.stack().iter().find(|s| s.kind() == Some(UiKind::Modal)) {
            ctx.accesskit_node_builder(dialog.unique_id, |node| node.set_label(title.clone()));
        }
        ui.label(strong(title));
        ui.add_space(12.0);
        body(ui);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            if primary(ui, yes).clicked() {
                answer = Some(true);
            }
            let no = ui.button(no);
            if !ctx.memory(|m| m.areas().visible_last_frame(&ui.layer_id())) {
                no.request_focus();
            }
            if no.clicked() {
                answer = Some(false);
            }
        });
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            answer = Some(false);
        }
    });
    answer
}

fn quality_name(q: Quality) -> String {
    t(match q {
        Quality::Balanced => "Balanced",
        Quality::Sharp => "Sharp text",
        Quality::Smooth => "Slow connection",
        Quality::Gaming => "Native (games)",
    })
}

/// The system, as a report names it: its name and version, and the processor. Asked once (on
/// Windows and macOS that runs a program).
fn system_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let os = format!("macOS {}", output("sw_vers", &["-productVersion"]));
        // The desktop and session pick the screen backend (portal, wlroots, X11).
        #[cfg(target_os = "linux")]
        let os = {
            let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
            let name = release.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).unwrap_or("Linux").trim_matches('"').to_string();
            let var = |k| std::env::var(k).unwrap_or_default();
            format!("{name}, {} on {}", var("XDG_CURRENT_DESKTOP"), var("XDG_SESSION_TYPE"))
        };
        // "Microsoft Windows [Version 10.0.22631.4317]"
        #[cfg(windows)]
        let os = output("cmd", &["/c", "ver"]);
        format!("{os} ({})", std::env::consts::ARCH)
    })
}

/// What a program prints, trimmed; empty if it can't run.
#[cfg(not(target_os = "linux"))]
fn output(program: &str, args: &[&str]) -> String {
    let mut c = std::process::Command::new(program);
    c.args(args);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// A new issue on Deskline's GitHub with its form filled in (`.github/ISSUE_TEMPLATE/problem.yml`;
/// the keys are its field ids). GitHub turns away addresses past about 8 KB, so a long story is
/// cut; the details never are.
fn issue_url(story: &str, about: &str) -> String {
    format!("https://github.com/luannzin/deskline/issues/new?template=problem.yml&what={}&system={}", encode(story, 6000), encode(about, 1000))
}

/// Percent-encoded for an address's query, cut at whole characters to at most `max` bytes.
fn encode(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let mut buf = [0; 4];
        let piece: String = c
            .encode_utf8(&mut buf)
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        if out.len() + piece.len() > max {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// Open a file, or a web address, with whatever this system opens it with.
fn open(path: &std::path::Path) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(path).spawn();
}

/// Messages and a line to type. Returns a line to send.
fn chat_panel(ui: &mut egui::Ui, chat: &mut Chat) -> Option<String> {
    let mut sent = None;
    egui::Panel::bottom(ui.scope_id().with("draft")).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8))).show(ui, |ui| {
        ui.horizontal(|ui| {
            let label = t("Send");
            let field = TextEdit::singleline(&mut chat.draft).hint_text(t("Message")).desired_width(look::beside(ui, &label));
            let r = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
            let clicked = primary(ui, label).clicked();
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (clicked || enter) && !chat.draft.trim().is_empty() {
                let text = std::mem::take(&mut chat.draft);
                chat.lines.push((true, text.clone()));
                sent = Some(text);
                r.request_focus();
            }
        });
    });
    ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
        for (mine, text) in &chat.lines {
            ui.label(muted(if *mine { t("You") } else { t("Them") }));
            ui.label(RichText::new(text).color(INK));
            ui.add_space(8.0);
        }
    });
    sent
}

/// Their folders, and sending yours. Top: where you are, as a trail of folders, the starting
/// places and Find. Middle: the folder's rows. Pinned at the bottom: sending, and the transfers.
fn files_panel(ui: &mut egui::Ui, v: &mut Viewing, control: bool, picked: &mpsc::UnboundedSender<Picked>) {
    if !control {
        ui.label(soft(t("File transfer needs control. Ask for control first.")));
        return;
    }
    let bottom = egui::Frame::new().inner_margin(egui::Margin { top: 8, ..Default::default() });
    egui::Panel::bottom(ui.scope_id().with("sending")).frame(bottom).show(ui, |ui| sending(ui, v, picked));

    let Some(listing) = v.listing.clone() else {
        look::waiting(ui, ui.available_width());
        return;
    };
    // Where you are: "Their computer › home › luan", any part opens it, Refresh at the end.
    let mut go = None;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if quiet(ui, t("Refresh")).clicked() {
                go = Some(listing.path.clone());
            }
            ui.with_layout(Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let parts = trail(&listing.path);
                let mut crumb = |ui: &mut egui::Ui, name: String, path: String, here: bool| {
                    if here {
                        ui.label(strong(name));
                    } else if quiet(ui, name).clicked() {
                        go = Some(path);
                    }
                };
                crumb(ui, t("Their computer"), String::new(), parts.is_empty());
                let last = parts.len().saturating_sub(1);
                for (i, (name, path)) in parts.into_iter().enumerate() {
                    ui.label(muted("›"));
                    crumb(ui, name, path, i == last);
                }
            });
        });
    });
    // The starting places, one click from anywhere.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (name, shown) in PLACES {
            if quiet(ui, t(shown)).clicked() {
                go = Some(name.into());
            }
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let label = ui.label(soft(t("Find")));
        ui.add(TextEdit::singleline(&mut v.find).desired_width(f32::INFINITY).min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)).labelled_by(label.id);
    });
    ui.add_space(8.0);
    look::hairline(ui);
    if let Some(path) = go {
        v.session.cmd(Cmd::List(path));
    }

    let find = v.find.trim().to_lowercase();
    let shown: Vec<_> = listing.entries.iter().filter(|e| find.is_empty() || e.name.to_lowercase().contains(&find)).collect();
    // The row under the pointer or with focus last frame shows Download (rows are sized before
    // they know).
    let hot_id = ui.scope_id().with("hot-row");
    let hot: Option<String> = ui.ctx().data(|d| d.get_temp(hot_id)).flatten();
    let mut now_hot = None;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.add_space(4.0);
        if shown.is_empty() {
            ui.label(soft(if listing.entries.is_empty() { t("This folder is empty.") } else { t("Nothing here matches.") }));
        }
        for e in shown {
            let path = listing.path_of(e);
            let name = if listing.path.is_empty() { place_name(&e.name) } else { e.name.clone() };
            let name = if e.dir { format!("{name}/") } else { name };
            let near = hot.as_deref() == Some(path.as_str());
            let mut download = false;
            // Every row takes the pointer and the keyboard: a folder opens on a click or Enter,
            // a file downloads on a double click or Enter.
            let (row, _) = look::row(ui, Sense::click(), |ui| {
                ui.horizontal(|ui| {
                    // As tall with Download as without, so nothing moves.
                    ui.set_min_height(30.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if near {
                            download = quiet(ui, t("Download")).on_hover_text(t("Into your Downloads")).clicked();
                        }
                        if !e.dir {
                            ui.label(muted(size(e.size)).monospace());
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&name).color(INK)).truncate());
                        });
                    });
                })
            });
            // The row is the control: a screen reader hears the name it opens or downloads.
            row.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
            if row.contains_pointer() || row.has_focus() {
                now_hot = Some(path.clone());
            }
            let enter = row.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if download || (!e.dir && (row.double_clicked() || enter)) {
                v.session.cmd(Cmd::Download(path));
            } else if e.dir && row.clicked() {
                v.session.cmd(Cmd::List(path));
            }
        }
    });
    if now_hot != hot {
        ui.ctx().data_mut(|d| d.insert_temp(hot_id, now_hot));
    }
}

/// The bottom of the Files panel: sending files to the folder on show (or their Downloads), and
/// each transfer with its progress, Cancel while it runs and Show when it came here.
fn sending(ui: &mut egui::Ui, v: &mut Viewing, picked: &mpsc::UnboundedSender<Picked>) {
    look::hairline(ui);
    ui.add_space(8.0);
    let to = v.send_to();
    ui.horizontal_wrapped(|ui| {
        let (files, folder) = if to.is_some() { (t("Send files here…"), t("Send a folder here…")) } else { (t("Send files…"), t("Send a folder…")) };
        let lands = if to.is_some() { t("Lands in the folder you're looking at.") } else { t("Lands in their Downloads.") };
        for (label, folders) in [(files, false), (folder, true)] {
            let button = if folders { quiet(ui, label) } else { ui.button(label) };
            if button.on_hover_text(lands.as_str()).clicked() {
                let (tx, id, to) = (picked.clone(), v.session.id, to.clone());
                tokio::spawn(async move {
                    let dialog = rfd::AsyncFileDialog::new();
                    let paths: Vec<PathBuf> = if folders {
                        dialog.pick_folder().await.into_iter().map(|f| f.path().to_path_buf()).collect()
                    } else {
                        dialog.pick_files().await.unwrap_or_default().iter().map(|f| f.path().to_path_buf()).collect()
                    };
                    for p in paths {
                        let _ = tx.send((id, p, to.clone()));
                    }
                });
            }
        }
    });
    ui.label(muted(t("Or drop them on the window.")));
    let mut cancel = None;
    for x in &v.transfers {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match &x.end {
                    None => {
                        if quiet(ui, t("Cancel")).clicked() {
                            cancel = Some(x.id);
                        }
                        ui.label(muted(percent(x)).monospace());
                    }
                    Some(Ok(at)) => {
                        if matches!(x.what, What::Get(_))
                            && quiet(ui, t("Show")).on_hover_text(at.as_str()).clicked()
                        {
                            open(std::path::Path::new(at).parent().unwrap_or(std::path::Path::new(at)));
                        }
                        let done = match x.what {
                            What::PasteHere(_) => t("Ready to paste"),
                            What::PasteThere(_) => t("Pasted"),
                            _ => t("Done"),
                        };
                        ui.label(muted(done));
                    }
                    Some(Err(why)) => {
                        ui.label(muted(t("Failed"))).on_hover_text(t(why));
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(what(&x.what)).color(INK)).truncate());
                });
            });
        });
        if x.end.is_none() {
            // A 2px line: the hairline, filled in soft ink as far as it's got.
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 2.0), Sense::hover());
            ui.painter().rect_filled(rect, 1.0, HAIRLINE);
            let done = rect.width() * x.done as f32 / x.total.max(1) as f32;
            ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(done.min(rect.width()), 2.0)), 1.0, SOFT);
        }
    }
    if let Some(id) = cancel {
        v.session.cmd(Cmd::Cancel(id));
        v.transfers.retain(|x| x.id != id);
    }
}

/// What a transfer moves, as the panel names it.
fn what(w: &What) -> String {
    match w {
        What::Send(name) | What::Get(name) => name.clone(),
        What::PasteThere(n) => tf("{} to their clipboard", &[&files_count(*n)]),
        What::PasteHere(n) => tf("{} to your clipboard", &[&files_count(*n)]),
    }
}

/// How far a transfer has got.
fn percent(x: &Transfer) -> String {
    format!("{}%", x.done.saturating_mul(100) / x.total.max(1))
}

fn files_count(n: usize) -> String {
    if n == 1 { t("1 file") } else { tf("{} files", &[&n.to_string()]) }
}

/// A starting place's name as it reads here ("Home" is "Home folder").
fn place_name(name: &str) -> String {
    PLACES.iter().find(|(n, _)| *n == name).map_or_else(|| name.to_string(), |(_, shown)| t(shown))
}

/// A folder path on their computer as the folders along it: each one's name and full path.
/// "/home/luan" is "/", "home", "luan"; "C:\Users" is "C:", "Users"; "\\server\share\x" starts
/// at the share.
fn trail(path: &str) -> Vec<(String, String)> {
    let unix = path.starts_with('/');
    let sep = if unix { '/' } else { '\\' };
    let mut parts = Vec::new();
    let mut at = String::new();
    let mut rest = path;
    if unix {
        parts.push(("/".to_string(), "/".to_string()));
        at.push('/');
    } else if let Some(unc) = path.strip_prefix("\\\\") {
        // The server and share together are the root of a network path.
        let mut s = unc.splitn(3, '\\');
        let (server, share) = (s.next().unwrap_or_default(), s.next().unwrap_or_default());
        at = format!("\\\\{server}\\{share}\\");
        parts.push((format!("\\\\{server}\\{share}"), at.clone()));
        rest = s.next().unwrap_or_default();
    }
    for name in rest.split(sep).filter(|p| !p.is_empty()) {
        if at.is_empty() {
            // A drive: "C:" opens as "C:\".
            at = format!("{name}{sep}");
        } else {
            if !at.ends_with(sep) {
                at.push(sep);
            }
            at.push_str(name);
        }
        parts.push((name.to_string(), at.clone()));
    }
    parts
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

/// Mouse over the picture and every key goes to the other screen. On Windows the keyboard hook
/// takes most keys first (Win, Alt+Tab and friends included) and egui never sees those; the rest
/// come through here. Windows silently drops a hook that answers slowly once, and then this is
/// how keys still get there.
fn forward_input(ui: &mut egui::Ui, v: &mut Viewing, rect: Rect, hovered: bool) {
    // Keys go to the remote unless a field (Chat, Find) is being typed in; a click on the
    // picture takes them back from it. Buttons never keep focus, or Space/Enter would press
    // them instead of reaching the remote.
    let typing = ui.ctx().egui_wants_keyboard_input() && !(hovered && ui.input(|i| i.pointer.any_pressed()));
    if !typing {
        ui.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
    }
    let send = |i: Input| v.session.send(Ask::Input(i));
    let at = |p: Pos2| {
        let n = (p - rect.min) / rect.size();
        Input::Move { x: n.x, y: n.y }
    };
    let key = |code: u16, down: bool| Ask::Input(Input::Key { code, down });
    let captured = v.captured.is_some();
    for e in ui.input(|i| i.events.clone()) {
        match e {
            // Captured, the mouse's raw counts move their pointer, wherever ours is: games turn
            // the camera with them and hide the pointer.
            UiEvent::MouseMoved(d) if captured => v.motion += d,
            UiEvent::PointerMoved(_) if captured => {}
            UiEvent::PointerButton { button, pressed, .. } if captured => {
                send_motion(&v.session, &mut v.motion);
                send(Input::Button { button: button_number(button), down: pressed });
            }
            UiEvent::Key { physical_key, key: k, pressed: true, modifiers: m, .. }
                if captured && m.ctrl && m.alt && m.shift && physical_key.unwrap_or(k) == Key::Z =>
            {
                v.captured = None;
            }
            UiEvent::PointerMoved(p) if rect.contains(p) => send(at(p)),
            UiEvent::PointerButton { pos, button, pressed, .. } if rect.contains(pos) || !pressed => {
                send(at(pos));
                send(Input::Button { button: button_number(button), down: pressed });
            }
            UiEvent::MouseWheel { unit, delta, .. } if hovered || captured => {
                let per_notch = match unit {
                    MouseWheelUnit::Line => 1.0,
                    MouseWheelUnit::Point => 1.0 / 50.0,
                    MouseWheelUnit::Page => 10.0,
                };
                v.scroll += delta * per_notch;
                let steps = Vec2::new(v.scroll.x.trunc(), v.scroll.y.trunc());
                if steps != Vec2::ZERO {
                    v.scroll -= steps;
                    send(Input::Scroll { x: -steps.x as i32, y: steps.y as i32 });
                }
            }
            _ if typing => {}
            UiEvent::Key { key: k, physical_key, pressed, .. } => {
                if let Some(code) = desktop::evdev(physical_key.unwrap_or(k)) {
                    v.session.send(key(code, pressed));
                }
            }
            // egui turns Ctrl+C/X/V into these instead of key events; Ctrl is already down remotely.
            // A paste may carry files: the session sends them over first (Cmd::Paste).
            UiEvent::Paste(_) => v.session.cmd(Cmd::Paste),
            UiEvent::Copy | UiEvent::Cut => {
                let code = if matches!(e, UiEvent::Copy) { 46 } else { 45 };
                v.session.send(key(code, true));
                v.session.send(key(code, false));
            }
            UiEvent::ModifiersChanged(m) => {
                // Command on a Mac is Ctrl on the other side.
                let (was_ctrl, now_ctrl) = (v.mods.ctrl || v.mods.mac_cmd, m.ctrl || m.mac_cmd);
                for (code, was, now) in [(SHIFT, v.mods.shift, m.shift), (CTRL, was_ctrl, now_ctrl), (ALT, v.mods.alt, m.alt)] {
                    if was != now {
                        v.session.send(key(code, now));
                    }
                }
                v.mods = m;
            }
            _ => {}
        }
    }
    // One message for all of this frame's movement: a fast mouse reports a thousand times a second.
    send_motion(&v.session, &mut v.motion);
}

/// The whole counts of `motion` go out; the fraction waits for more.
fn send_motion(session: &View, motion: &mut Vec2) {
    let step = Vec2::new(motion.x.trunc(), motion.y.trunc());
    if step != Vec2::ZERO {
        *motion -= step;
        session.send(Ask::Input(Input::MoveBy { dx: step.x as i32, dy: step.y as i32 }));
    }
}

/// `Input::Button`'s numbering.
fn button_number(b: PointerButton) -> u8 {
    match b {
        PointerButton::Primary => 0,
        PointerButton::Secondary => 1,
        PointerButton::Middle => 2,
        PointerButton::Extra1 => 3,
        PointerButton::Extra2 => 4,
    }
}

impl eframe::App for App {
    /// Runs even while the window is hidden or minimized, so requests still get through.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(e) = self.host_events.try_recv() {
            self.host_event(ctx, e);
        }
        while let Ok((id, e)) = self.view_events.try_recv() {
            self.view_event(ctx, id, e);
        }
        while let Ok((id, path, to)) = self.picked.1.try_recv() {
            if let Some(v) = self.views.iter().find(|v| v.session.id == id) {
                v.session.cmd(Cmd::Upload(path, to));
            }
        }
        self.housekeeping(ctx);
        self.short_code(ctx);
        self.float_bar(ctx);
        if self.hidden {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let current = match self.tab {
            Tab::View(id) => self.views.iter().position(|v| v.session.id == id),
            _ => None,
        };
        let fullscreen = current.is_some_and(|i| self.views[i].fullscreen);

        // The keyboard hook grabs while a screen we control is in front and focused.
        let want = current.is_some_and(|i| self.views[i].control == Control::On && self.views[i].panel != Panel::Chat)
            && ctx.input(|i| i.focused);
        #[cfg(windows)]
        {
            keyhook::grab(want);
            while let Ok((code, down)) = self.keys.try_recv() {
                // The hook takes the keys before egui sees them, so it watches for Ctrl+Alt+Shift+Z itself.
                let bit = match code {
                    CTRL | 97 => 1,
                    ALT | 100 => 2,
                    SHIFT | 54 => 4,
                    _ => 0,
                };
                if down {
                    self.hook_mods |= bit;
                } else {
                    self.hook_mods &= !bit;
                }
                // The hook only takes keys meant for the screen in front, and the releases of those
                // even once the grab has ended: all of them go, or a key stays down over there.
                if let Some(i) = current {
                    let v = &mut self.views[i];
                    if down && code == RELEASE_KEY && self.hook_mods == 7 && v.captured.is_some() {
                        v.captured = None;
                        continue;
                    }
                    // Ctrl+V may carry files: the session sends them over first.
                    if down && code == desktop::V && self.hook_mods == 1 {
                        v.session.cmd(Cmd::Paste);
                        continue;
                    }
                    v.session.send(Ask::Input(Input::Key { code, down }));
                }
            }
        }
        #[cfg(not(windows))]
        let _ = want;

        // A captured mouse stays with the window only while its screen is in front, controlled
        // and focused; anything else gives it back.
        let capture = current.is_some_and(|i| {
            let v = &self.views[i];
            v.captured.is_some() && v.control == Control::On && v.panel != Panel::Chat
        }) && ctx.input(|i| i.focused);
        if capture != self.grabbed {
            self.grabbed = capture;
            // Raw movement arrives either way. macOS can only lock the pointer in place, and X11
            // and Windows can only keep it in the window (asking them to lock drops the grab).
            let mode = if cfg!(target_os = "macos") { CursorGrab::Locked } else { CursorGrab::Confined };
            ctx.send_viewport_cmd(ViewportCommand::CursorGrab(if capture { mode } else { CursorGrab::None }));
            ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!capture));
        }

        if !fullscreen {
            self.tabs(ui);
        }
        self.banners(ui);
        match (self.tab, current) {
            (Tab::Settings, _) => {
                egui::CentralPanel::default().show(ui, |ui| self.settings(ui));
            }
            (Tab::View(_), Some(i)) if self.views[i].who.is_some() => self.viewer(ui, i),
            (Tab::View(_), Some(i)) => {
                egui::CentralPanel::default().show(ui, |ui| self.connecting(ui, i));
            }
            _ => {
                egui::CentralPanel::default().show(ui, |ui| self.home(ui));
            }
        }
        self.prompts(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_encode_and_cut_at_whole_characters() {
        assert_eq!(encode("a b/é~", 100), "a%20b%2F%C3%A9~");
        // é is 6 bytes encoded: it doesn't fit in the last 5, so it's left out whole
        assert_eq!(encode("ab é", 8), "ab%20");
        assert!(issue_url("x", "y").ends_with("template=problem.yml&what=x&system=y"));
    }

    #[test]
    fn trails_open_each_folder_along_the_way() {
        let pairs = |p: &str| trail(p).into_iter().map(|(n, p)| format!("{n}={p}")).collect::<Vec<_>>();
        assert_eq!(pairs("/home/luan/"), ["/=/", "home=/home", "luan=/home/luan"]);
        assert_eq!(pairs("/"), ["/=/"]);
        assert_eq!(pairs("C:\\Users\\luan"), ["C:=C:\\", "Users=C:\\Users", "luan=C:\\Users\\luan"]);
        assert_eq!(pairs("D:\\"), ["D:=D:\\"]);
        assert_eq!(pairs("\\\\nas\\share\\docs"), ["\\\\nas\\share=\\\\nas\\share\\", "docs=\\\\nas\\share\\docs"]);
        assert!(trail("").is_empty());
    }
}
