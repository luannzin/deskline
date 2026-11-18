//! System tray icon: Deskline keeps accepting sessions with its window closed. A badge shows
//! while someone waits for an answer.
//! Linux uses the pure-Rust StatusNotifierItem backend (no GTK).
// until ui.rs uses it
#![allow(dead_code)]

use eframe::egui::Context;
use std::sync::mpsc::{Receiver, channel};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub enum TrayMsg {
    Open,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    /// Plain, then badged.
    icons: [Icon; 2],
    alert: bool,
    tip: String,
    pub rx: Receiver<TrayMsg>,
}

fn icon(png: &[u8]) -> Icon {
    let img = image::load_from_memory(png).expect("embedded tray icon is a valid png").to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).expect("tray icon has a valid size")
}

impl Tray {
    /// None when the desktop has no tray. Call on the UI thread (macOS requires it).
    pub fn start(ctx: &Context) -> Option<Tray> {
        let open = MenuItem::new("Open Deskline", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &quit]).ok()?;
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());

        let (tx, rx) = channel();
        let (tx2, wake, wake2) = (tx.clone(), ctx.clone(), ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx.send(TrayMsg::Open);
                wake.request_repaint();
            }
        }));
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let msg = if e.id == open_id {
                TrayMsg::Open
            } else if e.id == quit_id {
                TrayMsg::Quit
            } else {
                return;
            };
            let _ = tx2.send(msg);
            wake2.request_repaint();
        }));

        let icons = [icon(include_bytes!("../assets/tray.png")), icon(include_bytes!("../assets/tray_alert.png"))];
        let built = TrayIconBuilder::new()
            .with_icon(icons[0].clone())
            .with_tooltip("Deskline")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build();
        match built {
            Ok(icon) => Some(Tray { icon, icons, alert: false, tip: "Deskline".into(), rx }),
            Err(e) => {
                eprintln!("deskline: no system tray ({e})");
                None
            }
        }
    }

    pub fn tooltip(&mut self, tip: &str) {
        if tip != self.tip {
            let _ = self.icon.set_tooltip(Some(tip));
            self.tip = tip.to_string();
        }
    }

    /// Badge the icon while someone waits for an answer.
    pub fn alert(&mut self, on: bool) {
        if on != self.alert {
            self.alert = on;
            let _ = self.icon.set_icon(Some(self.icons[on as usize].clone()));
        }
    }
}
