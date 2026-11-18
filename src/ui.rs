//! The window. Two jobs, two people: the person being helped sees their code, big, and one
//! question when someone wants in, then a red bar for as long as anyone is; the helper pastes a
//! code, waits on a line of dusk being drawn, and works in a tab per screen. Built from
//! luannzin.com's quiet system (look.rs), in English or Portuguese (i18n.rs).
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, Event as UiEvent, Id, Key, Layout, Modal, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, RichText,
    ScrollArea, Sense, TextEdit, TextureHandle, TextureOptions, Vec2, ViewportCommand, scroll_area::ScrollSource, vec2,
};
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;

use crate::codec::Quality;
use crate::desktop::{self, ALT, CTRL, Input, Monitor, SHIFT};
use crate::files::Listing;
use crate::host::{HostCmd, HostEvent};
use crate::i18n::{t, tf};
use crate::look::{self, GREEN, GROUND, HAIRLINE, INK, RED, SOFT, muted, primary, primary_enabled, quiet, soft, strong};
use crate::proto::Ask;
use crate::settings::{self, Peer, Store};
use crate::tray::{Tray, TrayMsg};
use crate::update::{self, Updater};
use crate::viewer::{self, Cmd, Options, Out, View, ViewEvent};
use crate::{autostart, net};
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

pub struct App {
    ep: Endpoint,
    store: Store,
    name: String,
    host: mpsc::UnboundedSender<HostCmd>,
    host_events: mpsc::UnboundedReceiver<HostEvent>,
    service: bool,
    out: Out,
    view_events: mpsc::UnboundedReceiver<(u64, ViewEvent)>,
    picked: (mpsc::UnboundedSender<(u64, PathBuf)>, mpsc::UnboundedReceiver<(u64, PathBuf)>),
    code: String,
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
    requests: Vec<Request>,
    shares: Vec<Share>,
    views: Vec<Viewing>,
    tab: Tab,
    form: Form,
    renaming: Option<(String, String)>,
    tray: Option<Tray>,
    hidden: bool,
    quitting: bool,
    updater: Option<Updater>,
    /// The pixel-art mark, drawn with nearest-neighbour so it stays crisp.
    mark: TextureHandle,
    #[cfg(windows)]
    keys: std::sync::mpsc::Receiver<(u16, bool)>,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Home,
    Settings,
    View(u64),
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
    mods: Modifiers,
    scroll: Vec2,
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
    transfers: Vec<(String, u64, u64)>,
    notes: Vec<String>,
    /// Typed while waiting to be accepted: log in with their password instead.
    password: String,
    remember: bool,
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
        let mut app = Self {
            name: net::whoami(),
            code: net::code(ep.id()),
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
            requests: Vec::new(),
            shares: Vec::new(),
            views: Vec::new(),
            tab: if settings { Tab::Settings } else { Tab::Home },
            form: Form { relay, ..Default::default() },
            renaming: None,
            tray,
            hidden,
            quitting: false,
            updater: (!service).then(|| Updater::start(home.join("update"))),
            mark: {
                let img = image::load_from_memory(include_bytes!("../assets/tray.png")).expect("the embedded mark is a png").to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                ctx.load_texture("mark", egui::ColorImage::from_rgba_unmultiplied(size, &img), TextureOptions::NEAREST)
            },
            #[cfg(windows)]
            keys: keyhook::start(ctx),
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
                self.shares.push(Share { id, who, control, asking: false, chat: Chat::default() });
                // Someone got in (perhaps with the password, perhaps trusted): the red bar shows.
                self.show(ctx);
            }
            HostEvent::ControlRequest { id } => {
                if let Some(s) = self.shares.iter_mut().find(|s| s.id == id) {
                    s.asking = true;
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
            }
            ViewEvent::Monitors { list, current } => (v.monitors, v.current) = (list, current),
            ViewEvent::Link { kind, ms } => {
                v.link = Some((kind, ms));
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
            ViewEvent::Listing(l) => v.listing = Some(l),
            ViewEvent::Transfer { name, done, total } => {
                v.transfers.retain(|x| x.0 != name);
                if done < total {
                    v.transfers.push((name, done, total));
                }
            }
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
            }
        }
    }

    fn connect(&mut self, code: &str, password: Option<String>, remember: bool) {
        let Some(peer) = net::parse_code(code) else {
            self.note = t("That code doesn't look right. It's 52 letters and digits.");
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
            mods: Modifiers::NONE,
            scroll: Vec2::ZERO,
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
            transfers: Vec::new(),
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
                if look::mark(ui, &self.mark, 32.0).interact(Sense::click()).clicked() {
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
                    tab(ui, &mut self.tab, Tab::View(v.session.id), label);
                    if look::close(ui).on_hover_text(t("Disconnect")).clicked() {
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
        // A note belongs to the page it was written on.
        if self.tab != before {
            self.note.clear();
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

    /// Home, one column: this computer's code for the person being helped, then the field for
    /// the helper, then the computers you've reached before, and the dusk strip to close it.
    fn home(&mut self, ui: &mut egui::Ui) {
        column(ui, |ui| {
            ui.add_space(40.0);
            ui.label(strong(t("Your code")));
            ui.label(soft(t("Send it to whoever is helping you. It never changes.")));
            ui.add_space(8.0);
            look::code(ui, &self.code);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let now = ui.input(|i| i.time);
                let copied = self.copied_at.is_some_and(|at| now - at < 2.0);
                if copied {
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                }
                if primary(ui, if copied { t("Copied") } else { t("Copy code") }).clicked() {
                    ui.ctx().copy_text(self.code.replace(' ', ""));
                    self.copied_at = Some(now);
                }
                ui.add_space(8.0);
                let state = if self.password_on { t("Unattended access is on") } else { t("Unattended access is off") };
                ui.label(muted(state));
                if quiet(ui, t("Change")).clicked() {
                    self.tab = Tab::Settings;
                }
            });

            ui.add_space(40.0);
            ui.label(strong(t("Connect to a computer")));
            ui.add_space(4.0);
            let mut go = false;
            // Button first from the right, so the field takes whatever width the words leave.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = !self.partner.trim().is_empty();
                let clicked = primary_enabled(ui, ready, t("Connect")).clicked();
                let field = TextEdit::singleline(&mut self.partner).hint_text(t("Paste their code")).desired_width(ui.available_width());
                let field = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
                go = (clicked || (field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && ready;
            });
            if self.need_password {
                let pw = TextEdit::singleline(&mut self.password).password(true).hint_text(t("Their unattended-access password"));
                let pw = ui.add(pw.desired_width(f32::INFINITY).min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
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
                ui.label(soft(&self.note));
            }
            self.update_row(ui);
            self.contacts(ui);
            ui.add_space(56.0);
            look::strip(ui, 24.0);
            ui.add_space(24.0);
        });
    }

    fn update_row(&mut self, ui: &mut egui::Ui) {
        let Some(u) = self.updater.as_mut() else { return };
        let Some((tag, _)) = u.ready.clone() else { return };
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.label(soft(tf("Deskline {} is ready.", &[&tag])));
            let label = if u.installing { t("Updating…") } else { t("Restart to update") };
            if primary_enabled(ui, !u.installing, label).clicked() {
                u.install();
            }
        });
    }

    fn contacts(&mut self, ui: &mut egui::Ui) {
        let contacts = self.store.get().contacts;
        if contacts.is_empty() {
            return;
        }
        ui.add_space(40.0);
        ui.label(strong(t("Recent")));
        ui.add_space(4.0);
        let mut connect = None;
        for c in &contacts {
            look::row(ui, |ui| {
                ui.horizontal(|ui| {
                    match self.renaming.as_mut().filter(|(id, _)| *id == c.id) {
                        Some((_, draft)) => {
                            let r = ui.add(TextEdit::singleline(draft).desired_width(200.0));
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
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if quiet(ui, t("Forget")).clicked() {
                            let id = c.id.clone();
                            let _ = self.store.update(|s| s.contacts.retain(|x| x.id != id));
                        }
                        if quiet(ui, t("Rename")).clicked() {
                            self.renaming = Some((c.id.clone(), c.alias.clone().unwrap_or_default()));
                        }
                        if ui.button(t("Connect")).clicked() {
                            connect = Some(c.id.clone());
                        }
                    });
                });
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
        column(ui, |ui| {
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
            if !self.note.is_empty() {
                ui.add_space(8.0);
                ui.label(soft(&self.note));
            }
            ui.add_space(32.0);
            ui.label(muted(format!("Deskline {}", env!("CARGO_PKG_VERSION"))));
            ui.add_space(24.0);
        });
    }

    /// Who gets in without a click. With the service, only an elevated window may change it.
    fn security(&mut self, ui: &mut egui::Ui) {
        ui.label(strong(t("Unattended access")));
        ui.label(soft(t(
            "With a password, people who know it can connect and control this computer when nobody is here to click Accept.",
        )));
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
            ui.add(secret(&mut self.form.pw, t("New password (8 characters or more)")));
            ui.add(secret(&mut self.form.pw2, t("Repeat it")));
            ui.horizontal(|ui| {
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
                look::row(ui, |ui| {
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
        ui.horizontal(|ui| {
            ui.label(soft(t("Picture")));
            ui.add_space(8.0);
            for q in [Quality::Balanced, Quality::Sharp, Quality::Smooth] {
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
            ui.label(soft(t("Installed: Deskline starts with Windows and works at the sign-in screen.")));
            if quiet(ui, t("Uninstall")).clicked() {
                std::thread::spawn(|| {
                    if service::elevate(&["--uninstall"]).is_ok() {
                        std::process::exit(0);
                    }
                });
            }
        } else {
            ui.horizontal(|ui| {
                if ui.button(t("Install for all users")).clicked() {
                    self.install();
                }
                ui.label(muted(t("Starts with Windows and works at the sign-in screen.")));
            });
        }
        ui.add_space(16.0);
        ui.label(strong(t("Relay server")));
        ui.label(soft(t(
            "Leave empty for the public relays. Your own: run iroh-relay and paste its URL. Restart Deskline after changing.",
        )));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let save = ui.button(t("Save")).clicked();
            let field = TextEdit::singleline(&mut self.form.relay).hint_text("https://relay.example.com").desired_width(ui.available_width());
            ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
            if save {
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
        ui.vertical_centered(|ui| {
            ui.set_max_width(360.0);
            ui.add_space((ui.available_height() * 0.22).max(40.0));
            ui.label(strong(&v.status));
            ui.label(muted(&net::code(v.peer)[..19]).monospace());
            ui.add_space(16.0);
            look::waiting(ui, 280.0);
            ui.add_space(24.0);
            if v.status == t("Waiting for them to accept…") {
                look::hairline(ui);
                ui.add_space(16.0);
                ui.label(soft(t("Or log in with their unattended-access password")));
                let field = TextEdit::singleline(&mut v.password).password(true).desired_width(f32::INFINITY);
                let r = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
                ui.checkbox(&mut v.remember, t("Log in without the password next time"));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (primary_enabled(ui, !v.password.is_empty(), t("Log in")).clicked() || enter) && !v.password.is_empty() {
                    login = Some((net::code(v.peer), v.password.clone(), v.remember));
                }
                ui.add_space(8.0);
            }
            cancel = quiet(ui, t("Cancel")).clicked();
        });
        if cancel || login.is_some() {
            let id = self.views[i].session.id;
            self.hang_up(ui.ctx(), id);
        }
        if let Some((code, pw, remember)) = login {
            self.connect(&code, Some(pw), remember);
        }
    }

    fn viewer(&mut self, ui: &mut egui::Ui, i: usize, grabbing: bool) {
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
                egui::Panel::right("files").frame(side).default_size(320.0).show(ui, |ui| files_panel(ui, v, control, &picked));
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

        // Files dropped on the window go to their Downloads.
        if control {
            for f in ui.input(|i| i.raw.dropped_files.clone()) {
                v.session.cmd(Cmd::Upload(f.path().to_path_buf()));
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
                        forward_input(ui, v, rect, response.hovered(), grabbing);
                    }
                });
            } else {
                let response = ui.allocate_rect(area, Sense::hover());
                let size = tex.size_vec2();
                let scale = (area.width() / size.x).min(area.height() / size.y);
                let rect = Rect::from_center_size(area.center(), size * scale);
                ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                if control {
                    forward_input(ui, v, rect, response.hovered(), grabbing);
                }
            }
            if !v.status.is_empty() {
                // Reconnecting: say so over the frozen picture, on a plate so it reads on anything.
                let galley = ui.painter().layout_no_wrap(v.status.clone(), egui::FontId::proportional(look::SIZE), INK);
                let plate = Rect::from_center_size(area.center_top() + vec2(0.0, 40.0), galley.size() + vec2(32.0, 16.0));
                ui.painter().rect_filled(plate, 10.0, Color32::from_black_alpha(200));
                ui.painter().galley(plate.center() - galley.size() / 2.0, galley, INK);
            }
        });

        if hang_up {
            let id = self.views[i].session.id;
            self.hang_up(ui.ctx(), id);
        }
    }

    /// While someone is in this computer: a red bar per person at the top of the window, with
    /// the way to stop them one click away; and while the window isn't in front, the same bar in
    /// a small window above every other one, so it's never behind what the helper opened.
    fn banners(&mut self, ui: &mut egui::Ui) {
        let mut cmds = Vec::new();
        for s in &mut self.shares {
            let frame = egui::Frame::new().fill(RED).inner_margin(egui::Margin::symmetric(16, 8));
            egui::Panel::top(Id::unique(("sharing", s.id))).frame(frame).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let what = if s.control { "{} sees and controls your screen" } else { "{} sees your screen" };
                    ui.label(strong(tf(what, &[&s.who])).color(Color32::WHITE));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let stop = RichText::new(t("Stop sharing")).color(RED);
                        if ui.add(egui::Button::new(stop).fill(Color32::WHITE).stroke(egui::Stroke::NONE)).clicked() {
                            cmds.push(HostCmd::Stop { id: s.id });
                        }
                        let on_red = |text: String| egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(Color32::from_white_alpha(28)).stroke(egui::Stroke::NONE);
                        if s.control && ui.add(on_red(t("Take back control"))).clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: false });
                        }
                        if !s.control && !s.asking && ui.add(on_red(t("Give control"))).clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: true });
                        }
                        let chat = if s.chat.unread > 0 { format!("{}  {}", t("Chat"), s.chat.unread) } else { t("Chat") };
                        if ui.add(on_red(chat)).clicked() {
                            s.chat.open = !s.chat.open;
                            s.chat.unread = 0;
                        }
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
        if !self.shares.is_empty() && !self.hidden && !ui.ctx().input(|i| i.focused) {
            let who = self.shares.iter().map(|s| s.who.as_str()).collect::<Vec<_>>().join(", ");
            let what = if self.shares.iter().any(|s| s.control) { "{} sees and controls your screen" } else { "{} sees your screen" };
            let line = tf(what, &[&who]);
            let monitor = ui.ctx().input(|i| i.viewport().monitor_size).unwrap_or(vec2(1280.0, 800.0));
            let size = vec2(560.0, 48.0);
            let bar = egui::ViewportBuilder::default()
                .with_title("Deskline")
                .with_always_on_top()
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_inner_size(size)
                .with_position(Pos2::new(((monitor.x - size.x) / 2.0).max(0.0), 12.0));
            let stop = ui.ctx().show_viewport_immediate(egui::ViewportId::from_hash_of("sharing-bar"), bar, |ui, _| {
                let mut stop = false;
                let frame = egui::Frame::new().fill(RED).inner_margin(egui::Margin::symmetric(16, 8));
                egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(strong(line.clone()).color(Color32::WHITE));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = RichText::new(t("Stop sharing")).color(RED);
                            stop = ui.add(egui::Button::new(label).fill(Color32::WHITE).stroke(egui::Stroke::NONE)).clicked();
                        });
                    });
                });
                stop
            });
            if stop {
                cmds.extend(self.shares.iter().map(|s| HostCmd::Stop { id: s.id }));
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

    /// The one question the person being helped has to answer, said plainly.
    fn prompts(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.requests.first_mut() {
            let mut answer = None;
            Modal::new(Id::unique(("request", r.req))).frame(prompt_frame()).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.label(strong(t("Someone wants to see your screen")));
                ui.add_space(12.0);
                ui.label(RichText::new(&r.who).size(20.0).color(INK));
                ui.label(soft(t("Only accept if you know who this is.")));
                ui.add_space(16.0);
                ui.checkbox(&mut r.control, t("Let them control (mouse, keyboard, clipboard, files)"));
                ui.checkbox(&mut r.trust, t("Always allow them, even when I'm away"));
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if primary(ui, t("Accept")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(t("Decline")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(yes) = answer {
                let r = self.requests.remove(0);
                let _ = self.host.send(HostCmd::Answer { req: r.req, yes, control: r.control && yes, trust: r.trust && yes });
            }
        }

        let Some(s) = self.shares.iter_mut().find(|s| s.asking) else { return };
        let mut answer = None;
        Modal::new(Id::unique(("control", s.id))).frame(prompt_frame()).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(strong(t("Control request")));
            ui.add_space(12.0);
            ui.label(RichText::new(tf("{} wants to control your mouse and keyboard.", &[&s.who])).color(INK));
            ui.label(soft(t("You can take control back at any time from the red bar.")));
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if primary(ui, t("Allow")).clicked() {
                    answer = Some(true);
                }
                if ui.button(t("Deny")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(yes) = answer {
            s.asking = false;
            let _ = self.host.send(HostCmd::AllowControl { id: s.id, yes });
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
}

/// The session's bar: who, the link drawn as dusk with its kind and round trip, control, the
/// session's tools, Disconnect at the end. A hairline under it.
fn toolbar(ui: &mut egui::Ui, v: &mut Viewing, hang_up: &mut bool) {
    let frame = egui::Frame::new().fill(GROUND).inner_margin(egui::Margin::symmetric(16, 8)).stroke(egui::Stroke::new(1.0, HAIRLINE));
    egui::Panel::top("viewer").frame(frame).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(strong(v.who.as_deref().unwrap_or("…")));
            let (quality, text) = match v.link {
                Some((kind, ms)) if v.status.is_empty() => (Some(look::link_quality(kind, ms)), format!("{}  {ms} ms", t(kind))),
                _ => (None, v.status.clone()),
            };
            look::link(ui, quality).on_hover_text(&text);
            ui.label(muted(text));
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
                }
            }
            if v.monitors.len() > 1 {
                let current = v.monitors.get(v.current).map_or(t("Screen"), |m| m.name.clone());
                let mut pick = v.current;
                egui::ComboBox::from_id_salt("monitor").selected_text(current).show_ui(ui, |ui| {
                    for (n, m) in v.monitors.iter().enumerate() {
                        ui.selectable_value(&mut pick, n, format!("{}  {}×{}", m.name, m.w, m.h));
                    }
                });
                if pick != v.current {
                    v.session.send(Ask::Monitor(pick));
                }
            }
            let mut q = v.quality;
            egui::ComboBox::from_id_salt("quality").selected_text(quality_name(q)).show_ui(ui, |ui| {
                for x in [Quality::Balanced, Quality::Sharp, Quality::Smooth] {
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
    });
}

/// A tab: ink with a hairline under it when it's the current one, soft otherwise.
fn tab(ui: &mut egui::Ui, current: &mut Tab, this: Tab, label: String) {
    let on = *current == this;
    let text = if on { strong(label) } else { soft(label) };
    let r = ui.add(egui::Label::new(text).sense(Sense::click()));
    if on {
        let y = r.rect.bottom() + 6.0;
        ui.painter().hline(r.rect.x_range(), y, egui::Stroke::new(1.0, INK));
    } else if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if r.clicked() {
        *current = this;
    }
}

/// luannzin.com's reading column: centred, left-aligned inside, scrolling as a whole.
fn column(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ScrollArea::vertical().show(ui, |ui| {
        let side = ((ui.available_width() - COLUMN) / 2.0).max(16.0);
        ui.horizontal(|ui| {
            ui.add_space(side);
            ui.vertical(|ui| {
                ui.set_max_width(COLUMN.min(ui.available_width() - 16.0));
                add(ui);
            });
        });
    });
}

/// A password field at the interface's height.
fn secret(s: &mut String, hint: String) -> TextEdit<'_> {
    TextEdit::singleline(s).password(true).hint_text(hint).min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER)
}

fn prompt_frame() -> egui::Frame {
    egui::Frame::new().fill(look::TILE).corner_radius(14).inner_margin(egui::Margin::same(24)).stroke(egui::Stroke::new(1.0, HAIRLINE))
}

fn quality_name(q: Quality) -> String {
    t(match q {
        Quality::Balanced => "Balanced",
        Quality::Sharp => "Sharp text",
        Quality::Smooth => "Slow connection",
    })
}

/// Open a file with whatever this system opens it with.
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
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let clicked = primary(ui, t("Send")).clicked();
            let field = TextEdit::singleline(&mut chat.draft).hint_text(t("Message")).desired_width(ui.available_width());
            let r = ui.add(field.min_size(vec2(0.0, 32.0)).align(egui::Align2::LEFT_CENTER));
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

/// Their folders, and sending yours.
fn files_panel(ui: &mut egui::Ui, v: &mut Viewing, control: bool, picked: &mpsc::UnboundedSender<(u64, PathBuf)>) {
    if !control {
        ui.label(soft(t("File transfer needs control. Ask for control first.")));
        return;
    }
    let id = v.session.id;
    ui.horizontal(|ui| {
        for folders in [false, true] {
            let label = if folders { t("Send a folder…") } else { t("Send files…") };
            if ui.button(label).on_hover_text(t("Lands in their Downloads. You can also drop files on the window.")).clicked() {
                let tx = picked.clone();
                tokio::spawn(async move {
                    let dialog = rfd::AsyncFileDialog::new();
                    let paths: Vec<PathBuf> = if folders {
                        dialog.pick_folder().await.into_iter().map(|f| f.path().to_path_buf()).collect()
                    } else {
                        dialog.pick_files().await.unwrap_or_default().iter().map(|f| f.path().to_path_buf()).collect()
                    };
                    for p in paths {
                        let _ = tx.send((id, p));
                    }
                });
            }
        }
    });
    for (name, done, total) in &v.transfers {
        ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).text(name.as_str()).fill(look::HOVER));
    }
    ui.add_space(8.0);
    look::hairline(ui);
    ui.add_space(8.0);
    let Some(listing) = v.listing.clone() else {
        look::waiting(ui, ui.available_width());
        return;
    };
    ui.horizontal(|ui| {
        if ui.add_enabled(!listing.path.is_empty(), egui::Button::new(soft(t("Up"))).frame_when_inactive(false)).clicked() {
            v.session.cmd(Cmd::List(listing.parent.clone().unwrap_or_default()));
        }
        let here = if listing.path.is_empty() { t("Their computer") } else { listing.path.clone() };
        ui.label(strong(here));
    });
    ScrollArea::vertical().show(ui, |ui| {
        for e in &listing.entries {
            let path = listing.path_of(e);
            look::row(ui, |ui| {
                ui.horizontal(|ui| {
                    if e.dir {
                        if ui.add(egui::Label::new(RichText::new(format!("{}/", e.name)).color(INK)).sense(Sense::click())).clicked() {
                            v.session.cmd(Cmd::List(path.clone()));
                        }
                    } else {
                        ui.label(RichText::new(&e.name).color(INK));
                        ui.label(muted(size(e.size)));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if quiet(ui, t("Download")).on_hover_text(t("Into your Downloads")).clicked() {
                            v.session.cmd(Cmd::Download(path.clone()));
                        }
                    });
                });
            });
        }
    });
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

/// Mouse over the picture and every key goes to the other screen. With the Windows keyboard
/// hook grabbing, keys come from the hook instead (Win, Alt+Tab and friends included).
fn forward_input(ui: &mut egui::Ui, v: &mut Viewing, rect: Rect, hovered: bool, grabbing: bool) {
    // Keep buttons from taking focus, or Space/Enter would press them instead of reaching the remote.
    if v.panel != Panel::Chat {
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
    let typing = v.panel == Panel::Chat && ui.memory(|m| m.focused().is_some());
    for e in ui.input(|i| i.events.clone()) {
        match e {
            UiEvent::PointerMoved(p) if rect.contains(p) => send(at(p)),
            UiEvent::PointerButton { pos, button, pressed, .. } if rect.contains(pos) || !pressed => {
                let button = match button {
                    PointerButton::Primary => 0,
                    PointerButton::Secondary => 1,
                    PointerButton::Middle => 2,
                    PointerButton::Extra1 => 3,
                    PointerButton::Extra2 => 4,
                };
                send(at(pos));
                send(Input::Button { button, down: pressed });
            }
            UiEvent::MouseWheel { unit, delta, .. } if hovered => {
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
            _ if grabbing || typing => {}
            UiEvent::Key { key: k, physical_key, pressed, .. } => {
                if let Some(code) = desktop::evdev(physical_key.unwrap_or(k)) {
                    v.session.send(key(code, pressed));
                }
            }
            // egui turns Ctrl+C/X/V into these instead of key events; Ctrl is already down remotely.
            UiEvent::Copy | UiEvent::Cut | UiEvent::Paste(_) => {
                let code = match e {
                    UiEvent::Copy => 46,
                    UiEvent::Cut => 45,
                    _ => 47,
                };
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
        while let Ok((id, path)) = self.picked.1.try_recv() {
            if let Some(v) = self.views.iter().find(|v| v.session.id == id) {
                v.session.cmd(Cmd::Upload(path));
            }
        }
        self.housekeeping(ctx);
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
        let grabbing = {
            keyhook::grab(want);
            let grabbing = keyhook::grabbing();
            while let Ok((code, down)) = self.keys.try_recv() {
                if let (true, Some(i)) = (grabbing, current) {
                    self.views[i].session.send(Ask::Input(Input::Key { code, down }));
                }
            }
            grabbing
        };
        #[cfg(not(windows))]
        let grabbing = {
            let _ = want;
            false
        };

        if !fullscreen {
            self.tabs(ui);
        }
        self.banners(ui);
        match (self.tab, current) {
            (Tab::Settings, _) => {
                egui::CentralPanel::default().show(ui, |ui| self.settings(ui));
            }
            (Tab::View(_), Some(i)) if self.views[i].who.is_some() => self.viewer(ui, i, grabbing),
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
