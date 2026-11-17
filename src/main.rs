// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod codec;
mod desktop;
mod net;
mod ui;

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let home = home()?;
    let rt = tokio::runtime::Runtime::new()?;
    let ep = rt.block_on(net::start(&home))?;
    let _rt = rt.enter();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Deskline")
            .with_app_id("deskline")
            .with_inner_size([520.0, 440.0])
            .with_min_inner_size([420.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native("Deskline", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc, ep, home)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Where the identity lives. `--home DIR` runs a second copy with its own code (for testing
/// on one machine).
fn home() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--home" {
            return args.next().map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("--home needs a folder"));
        }
    }
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    Ok(base.ok_or_else(|| anyhow::anyhow!("no home folder"))?.join("deskline"))
}
