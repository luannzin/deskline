//! The window: your code, a field for theirs, the prompts, and the remote screen.
use std::path::PathBuf;

use eframe::egui::{
    self, Align, Color32, Event as UiEvent, Id, Key, Layout, Modal, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    RichText, Sense, TextEdit, TextureHandle, TextureOptions, ViewportCommand,
};
use iroh::Endpoint;
use tokio::sync::{mpsc, oneshot};

use crate::desktop::{self, ALT, CTRL, Input, SHIFT};
use crate::net::{self, Ask, Event, Events, HostCmd, View};

pub struct App {
    ep: Endpoint,
    name: String,
    code: String,
    partner: String,
    events: Events,
    inbox: mpsc::UnboundedReceiver<Event>,
    note: String,
    request: Option<(String, oneshot::Sender<bool>)>,
    share: Option<Sharing>,
    view: Option<Viewing>,
}

struct Sharing {
    id: u64,
    who: String,
    ctl: mpsc::UnboundedSender<HostCmd>,
    control: bool,
    ask: Option<oneshot::Sender<bool>>,
}

#[derive(PartialEq)]
enum Control {
    Off,
    Asked,
    On,
}

struct Viewing {
    session: View,
    who: Option<String>,
    status: String,
    link: String,
    control: Control,
    tex: Option<TextureHandle>,
    mods: Modifiers,
    scroll: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, ep: Endpoint, home: PathBuf) -> Self {
        let (events, inbox) = Events::new(cc.egui_ctx.clone());
        let name = net::whoami();
        net::listen(ep.clone(), home, name.clone(), events.clone());
        let code = net::code(ep.id());
        Self { ep, name, code, partner: String::new(), events, inbox, note: String::new(), request: None, share: None, view: None }
    }

    fn handle(&mut self, ctx: &egui::Context, e: Event) {
        let viewing = |v: &Option<Viewing>, id| v.as_ref().is_some_and(|v| v.session.id == id);
        if matches!(e, Event::Request { .. } | Event::ControlRequest { .. }) {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
        }
        match e {
            Event::Request { who, reply } => self.request = Some((who, reply)),
            Event::Sharing { id, who, ctl } => {
                self.note.clear();
                self.share = Some(Sharing { id, who, ctl, control: false, ask: None });
            }
            Event::ControlRequest { id, reply } => {
                if let Some(s) = self.share.as_mut().filter(|s| s.id == id) {
                    s.ask = Some(reply);
                }
            }
            Event::ShareEnded { id, why } => {
                if id == 0 || self.share.as_ref().is_some_and(|s| s.id == id) {
                    self.share = None;
                    self.note = why;
                }
            }
            Event::Status { id, text } if viewing(&self.view, id) => self.view.as_mut().unwrap().status = text,
            Event::Viewing { id, who } if viewing(&self.view, id) => {
                self.view.as_mut().unwrap().who = Some(who);
                ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            }
            Event::Control { id, on } if viewing(&self.view, id) => {
                let v = self.view.as_mut().unwrap();
                if !on && v.control == Control::Asked {
                    self.note = "They said no to control.".into();
                }
                v.control = if on { Control::On } else { Control::Off };
            }
            Event::Link { id, text } if viewing(&self.view, id) => self.view.as_mut().unwrap().link = text,
            Event::ViewEnded { id, why } if viewing(&self.view, id) => self.end_view(ctx, why),
            _ => {}
        }
    }

    fn end_view(&mut self, ctx: &egui::Context, why: String) {
        if self.view.take().is_some_and(|v| v.who.is_some()) {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        self.note = why;
    }

    fn connect(&mut self) {
        let Some(peer) = net::parse_code(&self.partner) else {
            self.note = "That code doesn't look right. It's 52 letters and digits.".into();
            return;
        };
        if peer == self.ep.id() {
            self.note = "That's your own code. Paste your partner's.".into();
            return;
        }
        self.note.clear();
        let session = net::view(self.ep.clone(), peer, self.name.clone(), self.events.clone());
        self.view = Some(Viewing {
            session,
            who: None,
            status: "Connecting…".into(),
            link: String::new(),
            control: Control::Off,
            tex: None,
            mods: Modifiers::NONE,
            scroll: 0.0,
        });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.set_max_width(420.0);
            ui.add_space(24.0);
            ui.heading(RichText::new("Deskline").size(28.0).strong());
            ui.add_space(24.0);

            ui.label(RichText::new("Your code").strong());
            ui.add_space(4.0);
            ui.label(RichText::new(&self.code).monospace().size(15.0));
            ui.add_space(6.0);
            if ui.button("Copy my code").clicked() {
                ui.ctx().copy_text(self.code.replace(' ', ""));
                self.note = "Code copied. Send it to whoever needs to see this screen.".into();
            }

            ui.add_space(24.0);
            ui.separator();
            ui.add_space(16.0);

            ui.label(RichText::new("See a partner's screen").strong());
            ui.add_space(6.0);
            let field = ui.add(TextEdit::singleline(&mut self.partner).hint_text("Paste their code").desired_width(f32::INFINITY));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.add_space(6.0);
            let ready = !self.partner.trim().is_empty();
            if ui.add_enabled(ready, egui::Button::new("Connect")).clicked() || (enter && ready) {
                self.connect();
            }

            if !self.note.is_empty() {
                ui.add_space(16.0);
                ui.label(RichText::new(&self.note).weak());
            }
        });
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let Some(v) = self.view.as_mut() else { return };
        let mut hang_up = false;

        egui::Panel::top("viewer").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(v.who.as_deref().unwrap_or("…")).strong());
                ui.label(RichText::new(&v.link).weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    hang_up = ui.button("Disconnect").clicked();
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
                            ui.label(RichText::new("You have control").color(Color32::from_rgb(90, 200, 120)));
                        }
                    }
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            if let Some(img) = v.session.frame.lock().unwrap().take() {
                match v.tex.as_mut() {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => v.tex = Some(ui.ctx().load_texture("screen", img, TextureOptions::LINEAR)),
                }
            }
            let area = ui.available_rect_before_wrap();
            let response = ui.allocate_rect(area, Sense::hover());
            let Some(tex) = v.tex.as_ref() else {
                ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, "Waiting for the first picture…", egui::FontId::proportional(16.0), Color32::GRAY);
                return;
            };
            let size = tex.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let rect = Rect::from_center_size(area.center(), size * scale);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            if v.control == Control::On {
                forward_input(ui, v, rect, response.hovered());
            }
        });

        if hang_up {
            self.end_view(ui.ctx(), String::new());
        }
    }

    fn connecting(&mut self, ui: &mut egui::Ui) {
        let Some(v) = self.view.as_ref() else { return };
        let mut cancel = false;
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.spinner();
            ui.add_space(12.0);
            ui.label(&v.status);
            ui.add_space(12.0);
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            self.view = None;
        }
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.share.as_ref() else { return };
        let (mut revoke, mut stop) = (false, false);
        egui::Panel::top("sharing")
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(150, 40, 40)).inner_margin(8.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let what = if s.control { "sees and controls your screen" } else { "sees your screen" };
                    ui.label(RichText::new(format!("{} {what}", s.who)).color(Color32::WHITE).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        stop = ui.button("Stop sharing").clicked();
                        if s.control {
                            revoke = ui.button("Take back control").clicked();
                        }
                    });
                });
            });
        let s = self.share.as_mut().unwrap();
        if revoke {
            s.control = false;
            let _ = s.ctl.send(HostCmd::Revoke);
        }
        if stop {
            let _ = s.ctl.send(HostCmd::Stop);
        }
    }

    fn prompts(&mut self, ctx: &egui::Context) {
        if self.request.as_ref().is_some_and(|(_, r)| r.is_closed()) {
            self.request = None;
        }
        if let Some((who, _)) = self.request.as_ref() {
            let mut answer = None;
            Modal::new(Id::unique("request")).show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.heading("Screen request");
                ui.add_space(8.0);
                ui.label(format!("{who} wants to see your screen."));
                ui.label(RichText::new("Only accept if you know who this is.").weak());
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
                let (_, reply) = self.request.take().unwrap();
                let _ = reply.send(yes);
            }
        }

        let Some(s) = self.share.as_mut() else { return };
        if s.ask.as_ref().is_some_and(|r| r.is_closed()) {
            s.ask = None;
        }
        if s.ask.is_none() {
            return;
        }
        let mut answer = None;
        Modal::new(Id::unique("control")).show(ctx, |ui| {
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
            s.control = yes;
            let _ = s.ask.take().unwrap().send(yes);
        }
    }
}

/// Mouse over the picture and every key goes to the other screen.
fn forward_input(ui: &mut egui::Ui, v: &mut Viewing, rect: Rect, hovered: bool) {
    // Keep buttons from taking focus, or Space/Enter would press them instead of reaching the remote.
    ui.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    let send = |i: Input| v.session.send(Ask::Input(i));
    let at = |p: Pos2| {
        let n = (p - rect.min) / rect.size();
        Input::Move { x: n.x, y: n.y }
    };
    let key = |code: u16, down: bool| Ask::Input(Input::Key { code, down });
    for e in ui.input(|i| i.events.clone()) {
        match e {
            UiEvent::PointerMoved(p) if rect.contains(p) => send(at(p)),
            UiEvent::PointerButton { pos, button, pressed, .. } if rect.contains(pos) || !pressed => {
                let button = match button {
                    PointerButton::Primary => 0,
                    PointerButton::Secondary => 1,
                    PointerButton::Middle => 2,
                    _ => continue,
                };
                send(at(pos));
                send(Input::Button { button, down: pressed });
            }
            UiEvent::MouseWheel { unit, delta, .. } if hovered => {
                v.scroll += match unit {
                    MouseWheelUnit::Line => delta.y,
                    MouseWheelUnit::Point => delta.y / 50.0,
                    MouseWheelUnit::Page => delta.y * 10.0,
                };
                let steps = v.scroll.trunc();
                if steps != 0.0 {
                    v.scroll -= steps;
                    send(Input::Scroll { x: 0, y: steps as i32 });
                }
            }
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
                for (code, was, now) in [(SHIFT, v.mods.shift, m.shift), (CTRL, v.mods.ctrl, m.ctrl), (ALT, v.mods.alt, m.alt)] {
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
    /// Runs even while the window is minimized, so requests still flash the taskbar.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(e) = self.inbox.try_recv() {
            self.handle(ctx, e);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.banner(ui);
        match self.view.as_ref().map(|v| v.who.is_some()) {
            Some(true) => self.viewer(ui),
            Some(false) => {
                egui::CentralPanel::default().show(ui, |ui| self.connecting(ui));
            }
            None => {
                egui::CentralPanel::default().show(ui, |ui| self.home(ui));
            }
        }
        self.prompts(&ctx);
    }
}
