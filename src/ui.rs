//! The window: this computer's code, connecting to others (with an address book), the prompts
//! when someone wants in, red bars while sharing, a tab per screen you're viewing, and settings.
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, Event as UiEvent, Id, Key, Layout, Modal, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    RichText, ScrollArea, Sense, TextEdit, TextureHandle, TextureOptions, Vec2, ViewportCommand, scroll_area::ScrollSource,
};
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;

use crate::codec::Quality;
use crate::desktop::{self, ALT, CTRL, Input, Monitor, SHIFT};
use crate::files::Listing;
use crate::host::{HostCmd, HostEvent};
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

// luannzin.com's neutral system: near-black ground, paper ink in three tiers, hairlines, no
// accent colour. Colour means something here: red while sharing, green for control, and the
// dusk in the dither strip and the mark, which are art.
const GROUND: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
const TILE: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x1f);
const WELL: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);
const HAIRLINE: Color32 = Color32::from_rgb(0x24, 0x24, 0x24);
const RAISED: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
const HOVER: Color32 = Color32::from_rgb(0x32, 0x32, 0x32);
const PRESSED: Color32 = Color32::from_rgb(0x40, 0x40, 0x40);
const INK: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf5);
const SOFT: Color32 = Color32::from_rgb(0xb2, 0xb2, 0xb2);
const MUTED: Color32 = Color32::from_rgb(0x81, 0x81, 0x81);
const RED: Color32 = Color32::from_rgb(150, 40, 40);
const GREEN: Color32 = Color32::from_rgb(90, 200, 120);
/// The dusk of luannzin.com's dither strip, sky to horizon.
const DUSK: [Color32; 4] =
    [Color32::from_rgb(0x3a, 0x2a, 0x6b), Color32::from_rgb(0x7a, 0x33, 0x61), Color32::from_rgb(0xe0, 0x70, 0x3f), Color32::from_rgb(0xff, 0xd1, 0x80)];
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

fn theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    (v.panel_fill, v.window_fill, v.faint_bg_color, v.extreme_bg_color, v.code_bg_color) = (GROUND, TILE, TILE, WELL, WELL);
    v.window_stroke = egui::Stroke::new(1.0, HAIRLINE);
    v.hyperlink_color = INK;
    v.weak_text_color = Some(MUTED);
    v.selection.bg_fill = PRESSED;
    v.selection.stroke = egui::Stroke::new(1.0, INK);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, HAIRLINE);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, SOFT);
    for (s, fill) in [(&mut w.inactive, RAISED), (&mut w.hovered, HOVER), (&mut w.active, PRESSED), (&mut w.open, HOVER)] {
        (s.bg_fill, s.weak_bg_fill) = (fill, fill);
        s.fg_stroke = egui::Stroke::new(1.0, INK);
        s.bg_stroke = egui::Stroke::new(1.0, if fill == RAISED { HAIRLINE } else { PRESSED });
    }
    // Same look whatever the system's light or dark setting.
    ctx.all_styles_mut(|s| s.visuals = v.clone());
}

/// luannzin.com's footer strip: four blocks of the dusk, each denser than the last, 2px dots on
/// a 3px grid in a Bayer pattern.
fn dither_strip(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let (cols, rows) = ((rect.width() / 3.0) as usize, (height / 3.0) as usize);
    let painter = ui.painter_at(rect);
    for c in 0..cols {
        let block = (c * 4 / cols.max(1)).min(3);
        let fill = (block + 1) as f32 / 4.0;
        for r in 0..rows {
            if (BAYER[(r % 4) * 4 + c % 4] as f32) / 16.0 < fill {
                let at = rect.min + egui::vec2(c as f32 * 3.0, r as f32 * 3.0);
                painter.rect_filled(Rect::from_min_size(at, egui::vec2(2.0, 2.0)), 0.0, DUSK[block]);
            }
        }
    }
}

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
    link: String,
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
        theme(&ctx);
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
                self.note = format!("{who}: {text}");
            }
            HostEvent::ShareEnded { id, why } => {
                self.shares.retain(|s| s.id != id);
                self.note = why;
            }
            HostEvent::Error(e) => {
                self.form.error = e.clone();
                self.note = e;
            }
        }
    }

    fn view_event(&mut self, ctx: &egui::Context, id: u64, e: ViewEvent) {
        let Some(i) = self.views.iter().position(|v| v.session.id == id) else { return };
        let v = &mut self.views[i];
        match e {
            ViewEvent::Status(t) => v.status = t,
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
                    v.note("They said no to control.");
                }
                v.control = if on { Control::On } else { Control::Off };
            }
            ViewEvent::Monitors { list, current } => (v.monitors, v.current) = (list, current),
            ViewEvent::Link(t) => {
                v.link = t;
                v.status.clear();
            }
            ViewEvent::Chat(t) => {
                v.chat.lines.push((false, t));
                if v.panel != Panel::Chat {
                    v.chat.unread += 1;
                }
            }
            ViewEvent::Note(t) => v.note(&t),
            ViewEvent::Privacy(on) => v.privacy = on,
            ViewEvent::Listing(l) => v.listing = Some(l),
            ViewEvent::Transfer { name, done, total } => {
                v.transfers.retain(|t| t.0 != name);
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
                self.note = why;
            }
        }
    }

    fn connect(&mut self, code: &str, password: Option<String>, remember: bool) {
        let Some(peer) = net::parse_code(code) else {
            self.note = "That code doesn't look right. It's 52 letters and digits.".into();
            return;
        };
        if peer == self.ep.id() || self.code.replace(' ', "") == peer.to_z32() {
            self.note = "That's this computer's code. Paste your partner's.".into();
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
            status: "Connecting…".into(),
            link: String::new(),
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

    fn tabs(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Home, "Home");
                for v in &self.views {
                    let mut label = v.who.clone().unwrap_or_else(|| "Connecting…".into());
                    if v.chat.unread > 0 {
                        label.push_str(&format!(" ({})", v.chat.unread));
                    }
                    ui.selectable_value(&mut self.tab, Tab::View(v.session.id), label);
                    if ui.small_button("×").on_hover_text("Disconnect").clicked() {
                        close = Some(v.session.id);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                });
            });
        });
        if let Some(id) = close {
            self.hang_up(ui.ctx(), id);
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

    fn home(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(460.0);
                ui.add_space(20.0);
                ui.add(egui::Image::new(&self.mark).fit_to_exact_size(egui::vec2(64.0, 64.0)));
                ui.label(RichText::new("Deskline").size(22.0).color(INK));
                ui.add_space(16.0);

                card(ui, |ui| {
                    ui.label(RichText::new("This computer").strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.code).monospace().size(15.0));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Copy my code").clicked() {
                            ui.ctx().copy_text(self.code.replace(' ', ""));
                            self.note = "Code copied. Send it to whoever needs to see this screen.".into();
                        }
                        let state = if self.password_on { "Unattended access is on" } else { "Unattended access is off" };
                        if ui.link(state).clicked() {
                            self.tab = Tab::Settings;
                        }
                    });
                    #[cfg(windows)]
                    if !self.service {
                        ui.add_space(6.0);
                        if ui.button("Install Deskline on this computer").on_hover_text("Starts with Windows and works at the sign-in screen, lock screen and admin prompts").clicked() {
                            self.install();
                        }
                    }
                });

                ui.add_space(12.0);
                card(ui, |ui| {
                    ui.label(RichText::new("See a partner's screen").strong());
                    ui.add_space(6.0);
                    let field = ui.add(TextEdit::singleline(&mut self.partner).hint_text("Paste their code").desired_width(f32::INFINITY));
                    let mut go = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if self.need_password {
                        ui.add_space(6.0);
                        let pw = ui.add(TextEdit::singleline(&mut self.password).password(true).hint_text("Their unattended-access password").desired_width(f32::INFINITY));
                        go |= pw.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.checkbox(&mut self.remember, "Log in without the password next time");
                    }
                    ui.add_space(6.0);
                    let ready = !self.partner.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new("Connect")).clicked() || (go && ready) {
                        let password = (self.need_password && !self.password.is_empty()).then(|| self.password.clone());
                        let (code, remember) = (self.partner.clone(), self.remember);
                        self.connect(&code, password, remember);
                    }
                });

                if !self.note.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new(&self.note).weak());
                }
                self.update_banner(ui);
                self.contacts(ui);
                ui.add_space(32.0);
                dither_strip(ui, 32.0);
                ui.add_space(16.0);
            });
        });
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(u) = self.updater.as_mut() else { return };
        let Some((tag, _)) = u.ready.clone() else { return };
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(format!("Deskline {tag} is ready."));
            let label = if u.installing { "Updating…" } else { "Restart to update" };
            if ui.add_enabled(!u.installing, egui::Button::new(label)).clicked() {
                u.install();
            }
        });
    }

    fn contacts(&mut self, ui: &mut egui::Ui) {
        let contacts = self.store.get().contacts;
        if contacts.is_empty() {
            return;
        }
        ui.add_space(16.0);
        ui.label(RichText::new("Recent").strong());
        ui.add_space(4.0);
        let mut connect = None;
        for c in &contacts {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    match self.renaming.as_mut().filter(|(id, _)| *id == c.id) {
                        Some((_, draft)) => {
                            let r = ui.add(TextEdit::singleline(draft).desired_width(160.0));
                            if r.lost_focus() || ui.button("Save").clicked() {
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
                                ui.label(RichText::new(c.label()).strong());
                                ui.label(RichText::new(format!("{} · {}", &c.id[..8], settings::when(c.last))).weak().small());
                            });
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Forget").clicked() {
                            let id = c.id.clone();
                            let _ = self.store.update(|s| s.contacts.retain(|x| x.id != id));
                        }
                        if ui.small_button("Rename").clicked() {
                            self.renaming = Some((c.id.clone(), c.alias.clone().unwrap_or_default()));
                        }
                        if ui.button("Connect").clicked() {
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
        self.note = "Installing… confirm the Windows prompt.".into();
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.set_max_width(520.0);
                ui.add_space(16.0);
                ui.heading("Settings");
                ui.add_space(12.0);
                card(ui, |ui| self.security(ui));
                ui.add_space(10.0);
                card(ui, |ui| self.preferences(ui));
                ui.add_space(10.0);
                card(ui, |ui| self.system(ui));
                ui.add_space(10.0);
                ui.label(RichText::new(format!("Deskline {}", env!("CARGO_PKG_VERSION"))).weak());
            });
        });
    }

    /// Who gets in without a click. With the service, only an elevated window may change it.
    fn security(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Unattended access").strong());
        ui.label(
            RichText::new("With a password, people who know it can connect and control this computer when nobody is here to click Accept.")
                .weak(),
        );
        ui.add_space(6.0);
        #[cfg(windows)]
        let locked = self.service && !service::is_elevated();
        #[cfg(not(windows))]
        let locked = false;
        if locked {
            ui.label(if self.password_on { "A password is set." } else { "No password is set." });
            if ui.button("Change as administrator").clicked() {
                #[cfg(windows)]
                std::thread::spawn(|| service::elevate(&["--settings"]));
            }
        } else {
            ui.label(if self.password_on { "A password is set. Set a new one to replace it." } else { "No password is set." });
            ui.add(TextEdit::singleline(&mut self.form.pw).password(true).hint_text("New password (8 characters or more)"));
            ui.add(TextEdit::singleline(&mut self.form.pw2).password(true).hint_text("Repeat it"));
            ui.horizontal(|ui| {
                if ui.button("Set password").clicked() {
                    if self.form.pw.chars().count() < 8 {
                        self.form.error = "Use at least 8 characters.".into();
                    } else if self.form.pw != self.form.pw2 {
                        self.form.error = "The two passwords are different.".into();
                    } else {
                        let _ = self.host.send(HostCmd::SetPassword(Some(std::mem::take(&mut self.form.pw))));
                        self.form.pw2.clear();
                        self.form.error = "Password set.".into();
                    }
                }
                if self.password_on && ui.button("Turn off").clicked() {
                    let _ = self.host.send(HostCmd::SetPassword(None));
                    self.form.error = "Unattended access is off.".into();
                }
            });
            if !self.form.error.is_empty() {
                ui.label(RichText::new(&self.form.error).weak());
            }
        }
        if !self.trusted.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Always allowed").strong());
            for p in self.trusted.clone() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} ({})", p.name, &p.id[..8]));
                    if ui.add_enabled(!locked, egui::Button::new("Remove").small()).clicked() {
                        let _ = self.host.send(HostCmd::Untrust(p.id.clone()));
                    }
                });
            }
        }
    }

    fn preferences(&mut self, ui: &mut egui::Ui) {
        let mut s = self.store.get();
        let before = s.clone();
        ui.label(RichText::new("Sessions").strong());
        ui.checkbox(&mut s.allow_control, "When I accept someone, let them control by default");
        ui.checkbox(&mut s.sound, "Hear the other computer's sound");
        ui.horizontal(|ui| {
            ui.label("Picture:");
            ui.radio_value(&mut s.quality, Quality::Balanced, "Balanced");
            ui.radio_value(&mut s.quality, Quality::Sharp, "Sharp text");
            ui.radio_value(&mut s.quality, Quality::Smooth, "Slow connection");
        });
        if s != before {
            let _ = self.store.update(|x| *x = s);
        }
    }

    fn system(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("This computer").strong());
        if !self.service {
            let mut on = autostart::enabled();
            if ui.checkbox(&mut on, "Start Deskline when I sign in").changed() {
                if let Err(e) = autostart::set(on) {
                    self.note = format!("Couldn't change that: {e}");
                }
            }
        }
        #[cfg(windows)]
        if self.service {
            ui.label("Installed: Deskline starts with Windows and works at the sign-in screen.");
            if ui.button("Uninstall").clicked() {
                std::thread::spawn(|| {
                    if service::elevate(&["--uninstall"]).is_ok() {
                        std::process::exit(0);
                    }
                });
            }
        } else if ui.button("Install for all users (starts with Windows, works at the sign-in screen)").clicked() {
            self.install();
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Relay server").strong());
        ui.label(RichText::new("Leave empty for the public relays. Your own: run iroh-relay and paste its URL. Restart Deskline after changing.").weak());
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut self.form.relay).hint_text("https://relay.example.com").desired_width(280.0));
            if ui.button("Save").clicked() {
                let relay = Some(self.form.relay.trim().to_string()).filter(|r| !r.is_empty());
                let _ = self.store.update(|s| s.relay = relay);
                self.note = "Saved. Restart Deskline to use it.".into();
            }
        });
        ui.add_space(8.0);
        if self.service {
            ui.label(RichText::new("Session log: %ProgramData%\\Deskline\\sessions.log (administrators)").weak());
        } else if ui.button("Open session log").clicked() {
            open(&self.store.dir().join("sessions.log"));
        }
    }

    fn connecting(&mut self, ui: &mut egui::Ui, i: usize) {
        let mut cancel = false;
        let mut login = None;
        let v = &mut self.views[i];
        ui.vertical_centered(|ui| {
            ui.set_max_width(360.0);
            ui.add_space(80.0);
            ui.spinner();
            ui.add_space(12.0);
            ui.label(&v.status);
            ui.add_space(16.0);
            if v.status.starts_with("Waiting") {
                ui.label(RichText::new("Or log in with their unattended-access password:").weak());
                let r = ui.add(TextEdit::singleline(&mut v.password).password(true).desired_width(f32::INFINITY));
                ui.checkbox(&mut v.remember, "Log in without the password next time");
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.button("Log in").clicked() || enter) && !v.password.is_empty() {
                    login = Some((net::code(v.peer), v.password.clone(), v.remember));
                }
                ui.add_space(8.0);
            }
            cancel = ui.button("Cancel").clicked();
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
            egui::Panel::top("viewer").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(v.who.as_deref().unwrap_or("…")).strong());
                    let status = if v.status.is_empty() { &v.link } else { &v.status };
                    ui.label(RichText::new(status).weak());
                    ui.separator();
                    match v.control {
                        Control::Off => {
                            if ui.button("Ask for control").clicked() {
                                v.session.send(Ask::WantControl);
                                v.control = Control::Asked;
                            }
                        }
                        Control::Asked => {
                            ui.label("Waiting for them to allow control…");
                        }
                        Control::On => {
                            ui.label(RichText::new("You have control").color(GREEN));
                            if ui.button("Ctrl+Alt+Del").clicked() {
                                v.session.send(Ask::Input(Input::CtrlAltDel));
                            }
                            let label = if v.privacy { "Privacy: on" } else { "Privacy: off" };
                            if ui.button(label).on_hover_text("Blank their screen and lock their own mouse and keyboard").clicked() {
                                v.session.send(Ask::Privacy(!v.privacy));
                            }
                        }
                    }
                    if v.monitors.len() > 1 {
                        let current = v.monitors.get(v.current).map_or("Screen".into(), |m| m.name.clone());
                        let mut pick = v.current;
                        egui::ComboBox::from_id_salt("monitor").selected_text(current).show_ui(ui, |ui| {
                            for (n, m) in v.monitors.iter().enumerate() {
                                ui.selectable_value(&mut pick, n, format!("{} ({}×{})", m.name, m.w, m.h));
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
                    if ui.selectable_label(v.sound, "Sound").clicked() {
                        v.sound = !v.sound;
                        v.session.send(Ask::Sound(v.sound));
                    }
                    if ui.selectable_label(v.actual_size, "Actual size").clicked() {
                        v.actual_size = !v.actual_size;
                    }
                    if ui.button("Full screen").on_hover_text("F11 to leave").clicked() {
                        v.fullscreen = true;
                        ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(true));
                    }
                    let files = ui.selectable_label(v.panel == Panel::Files, "Files").on_hover_text("Needs control");
                    if files.clicked() {
                        v.panel = if v.panel == Panel::Files { Panel::None } else { Panel::Files };
                        if v.panel == Panel::Files && v.listing.is_none() {
                            v.session.cmd(Cmd::List(String::new()));
                        }
                    }
                    let chat = if v.chat.unread > 0 { format!("Chat ({})", v.chat.unread) } else { "Chat".into() };
                    if ui.selectable_label(v.panel == Panel::Chat, chat).clicked() {
                        v.panel = if v.panel == Panel::Chat { Panel::None } else { Panel::Chat };
                    }
                    hang_up = ui.button("Disconnect").clicked();
                });
                for n in &v.notes {
                    ui.label(RichText::new(n).weak().small());
                }
            });
        }
        if v.fullscreen && ui.input(|i| i.key_pressed(Key::F11)) {
            v.fullscreen = false;
            ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(false));
        }

        match v.panel {
            Panel::Files => {
                egui::Panel::right("files").default_size(300.0).show(ui, |ui| files_panel(ui, v, control, &picked));
            }
            Panel::Chat => {
                v.chat.unread = 0;
                egui::Panel::right("chat").default_size(280.0).show(ui, |ui| {
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
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => v.tex = Some(ui.ctx().load_texture("screen", img, TextureOptions::LINEAR)),
                }
            }
            let area = ui.available_rect_before_wrap();
            let Some(tex) = v.tex.clone() else {
                ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, "Waiting for the first picture…", egui::FontId::proportional(16.0), Color32::GRAY);
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
                ui.painter().text(area.center_top() + egui::vec2(0.0, 24.0), egui::Align2::CENTER_TOP, &v.status, egui::FontId::proportional(18.0), Color32::YELLOW);
            }
        });

        if hang_up {
            let id = self.views[i].session.id;
            self.hang_up(ui.ctx(), id);
        }
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        let mut cmds = Vec::new();
        for s in &mut self.shares {
            egui::Panel::top(Id::unique(("sharing", s.id))).frame(egui::Frame::NONE.fill(RED).inner_margin(8.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let what = if s.control { "sees and controls your screen" } else { "sees your screen" };
                    ui.label(RichText::new(format!("{} {what}", s.who)).color(Color32::WHITE).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Stop sharing").clicked() {
                            cmds.push(HostCmd::Stop { id: s.id });
                        }
                        if s.control && ui.button("Take back control").clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: false });
                        }
                        if !s.control && !s.asking && ui.button("Give control").clicked() {
                            cmds.push(HostCmd::AllowControl { id: s.id, yes: true });
                        }
                        let chat = if s.chat.unread > 0 { format!("Chat ({})", s.chat.unread) } else { "Chat".into() };
                        if ui.button(chat).clicked() {
                            s.chat.open = !s.chat.open;
                            s.chat.unread = 0;
                        }
                    });
                });
            });
            if s.chat.open {
                let mut open = true;
                egui::Window::new(format!("Chat with {}", s.who)).id(Id::unique(("chat", s.id))).open(&mut open).default_size([320.0, 300.0]).show(ui.ctx(), |ui| {
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

    fn prompts(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.requests.first_mut() {
            let mut answer = None;
            Modal::new(Id::unique(("request", r.req))).show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.heading("Someone wants to see your screen");
                ui.add_space(8.0);
                ui.label(RichText::new(&r.who).strong());
                ui.label(RichText::new("Only accept if you know who this is.").weak());
                ui.add_space(8.0);
                ui.checkbox(&mut r.control, "Let them control (mouse, keyboard, clipboard, files)");
                ui.checkbox(&mut r.trust, "Always allow them, even when I'm away");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Accept").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Decline").clicked() {
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
        Modal::new(Id::unique(("control", s.id))).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading("Control request");
            ui.add_space(8.0);
            ui.label(format!("{} wants to control your mouse and keyboard.", s.who));
            ui.label(RichText::new("You can take control back at any time from the red bar.").weak());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Allow").clicked() {
                    answer = Some(true);
                }
                if ui.button("Deny").clicked() {
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
        if let Some(t) = &self.tray {
            while let Ok(m) = t.rx.try_recv() {
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
        if let Some(t) = self.tray.as_mut() {
            let waiting = !self.requests.is_empty() || self.shares.iter().any(|s| s.asking);
            t.alert(waiting);
            t.tooltip(&match self.shares.len() {
                0 => "Deskline".to_string(),
                n => format!("Deskline: {n} watching"),
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
                        self.note = format!("Update failed: {e}");
                    }
                }
            }
        }
        // Closing keeps this computer reachable from the tray, unless the service does that.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.tray.is_some() && self.views.is_empty() {
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

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical(add);
    });
}

fn quality_name(q: Quality) -> &'static str {
    match q {
        Quality::Balanced => "Balanced",
        Quality::Sharp => "Sharp text",
        Quality::Smooth => "Slow connection",
    }
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
    egui::Panel::bottom(ui.scope_id().with("draft")).show(ui, |ui| {
        ui.horizontal(|ui| {
            let r = ui.add(TextEdit::singleline(&mut chat.draft).hint_text("Message").desired_width(ui.available_width() - 60.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.button("Send").clicked() || enter) && !chat.draft.trim().is_empty() {
                let text = std::mem::take(&mut chat.draft);
                chat.lines.push((true, text.clone()));
                sent = Some(text);
                r.request_focus();
            }
        });
    });
    ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
        for (mine, text) in &chat.lines {
            let who = if *mine { "You" } else { "Them" };
            ui.label(RichText::new(who).strong().small());
            ui.label(text);
            ui.add_space(4.0);
        }
    });
    sent
}

/// Their folders, and sending yours.
fn files_panel(ui: &mut egui::Ui, v: &mut Viewing, control: bool, picked: &mpsc::UnboundedSender<(u64, PathBuf)>) {
    if !control {
        ui.label("File transfer needs control. Ask for control first.");
        return;
    }
    let id = v.session.id;
    ui.horizontal(|ui| {
        for folders in [false, true] {
            let label = if folders { "Send a folder…" } else { "Send files…" };
            if ui.button(label).on_hover_text("Lands in their Downloads. You can also drop files on the window.").clicked() {
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
        ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).text(name.as_str()));
    }
    ui.separator();
    let Some(listing) = v.listing.clone() else {
        ui.spinner();
        return;
    };
    ui.horizontal(|ui| {
        if ui.add_enabled(!listing.path.is_empty(), egui::Button::new("Up").small()).clicked() {
            v.session.cmd(Cmd::List(listing.parent.clone().unwrap_or_default()));
        }
        ui.label(RichText::new(if listing.path.is_empty() { "Their computer" } else { &listing.path }).strong());
    });
    ScrollArea::vertical().show(ui, |ui| {
        for e in &listing.entries {
            ui.horizontal(|ui| {
                let path = listing.path_of(e);
                if e.dir {
                    if ui.link(format!("{}/", e.name)).clicked() {
                        v.session.cmd(Cmd::List(path.clone()));
                    }
                } else {
                    ui.label(format!("{}  {}", e.name, size(e.size)));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Download").on_hover_text("Into your Downloads").clicked() {
                        v.session.cmd(Cmd::Download(path));
                    }
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
