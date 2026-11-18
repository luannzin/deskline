// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod clip;
mod codec;
mod desktop;
mod files;
mod host;
mod i18n;
#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod keyhook;
mod look;
mod net;
mod proto;
#[cfg(windows)]
mod service;
mod settings;
mod tray;
mod ui;
mod update;
mod viewer;

use std::path::{Path, PathBuf};

use tokio::sync::{mpsc, watch};

use crate::host::{HostCmd, HostEvent, UiLink};
use crate::settings::Store;

/// `deskline`                 the window (`--home DIR`: another identity, for testing on one machine;
///                            `--background`: start in the tray; `--settings`: open on Settings)
/// `deskline CODE`            connect to CODE (`--with-password`: read its password from stdin)
/// `deskline --version`       print the version (the updater checks downloads with it)
/// Windows only:
/// `deskline --install`       install for all users with the service (asks for admin)
/// `deskline --uninstall`     remove it (asks for admin)
/// `deskline --service`       the service itself, started by Windows
/// `deskline --server`        the host side, started by the service as SYSTEM in the user's session
/// `deskline --restart-service`  used by the updater
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    #[cfg(windows)]
    {
        if flag("--service") {
            return service::run();
        }
        if flag("--server") {
            return server();
        }
        if flag("--restart-service") {
            return service::restart_service();
        }
        let steps: [(&str, fn() -> anyhow::Result<()>, &str); 2] = [
            ("--install", service::install, "Deskline is installed. It starts with Windows and works at the sign-in screen."),
            ("--uninstall", service::uninstall, "Deskline was removed from this computer."),
        ];
        for (f, run, done) in steps {
            if flag(f) {
                if !service::is_elevated() {
                    // The elevated copy reports how it went.
                    let _ = service::elevate(&[f]);
                    return Ok(());
                }
                // No console in release builds: say it in a box.
                match run() {
                    Ok(()) if !flag("--quiet") => message(done),
                    Ok(()) => {}
                    Err(e) => {
                        message(&format!("{e:#}"));
                        // The window that asked waits on this exit code.
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
        }
    }
    update::cleanup();
    let home = home(&args)?;
    // `deskline CODE` or `--connect CODE` opens that screen right away; `--with-password` reads
    // the password from stdin (`echo pw | deskline CODE --with-password`, like AnyDesk), so it
    // never shows up in a process list.
    let code = args.iter().position(|a| a == "--connect").and_then(|i| args.get(i + 1)).or_else(|| args.iter().find(|a| net::parse_code(a).is_some()));
    let connect = code.map(|c| {
        let mut pw = String::new();
        if flag("--with-password") {
            let _ = std::io::stdin().read_line(&mut pw);
        }
        let pw = pw.trim_end_matches(['\r', '\n']).to_string();
        (c.clone(), Some(pw).filter(|p| !p.is_empty()))
    });
    window(home, flag("--background"), flag("--settings"), connect)
}

/// Where the identity and settings live.
fn home(args: &[String]) -> anyhow::Result<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--home") {
        return args.get(i + 1).map(PathBuf::from).ok_or_else(|| anyhow::anyhow!("--home needs a folder"));
    }
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("APPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    Ok(base.ok_or_else(|| anyhow::anyhow!("no home folder"))?.join("deskline"))
}

fn window(home: PathBuf, background: bool, settings: bool, connect: Option<(String, Option<String>)>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let store = Store::open(&home);
    let ep = rt.block_on(net::start(&home, store.get().relay.as_deref()))?;
    let _rt = rt.enter();
    let (host, events, service) = rt.block_on(link_host(&ep, &home, &store));

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Deskline")
            .with_app_id("deskline")
            .with_inner_size([760.0, 560.0])
            .with_min_inner_size([420.0, 380.0])
            .with_icon(std::sync::Arc::new(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("the embedded icon is a png")))
            .with_visible(!background),
        ..Default::default()
    };
    let start = ui::Start { ep, home, store, host, events, service, background, settings, connect };
    eframe::run_native("Deskline", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc, start))))).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The host side this window shows: the installed service's (Windows), else its own.
async fn link_host(
    ep: &iroh::Endpoint,
    home: &Path,
    store: &Store,
) -> (mpsc::UnboundedSender<HostCmd>, mpsc::UnboundedReceiver<HostEvent>, bool) {
    #[cfg(windows)]
    if service::is_installed() {
        // Just after boot or an update the server may still be starting.
        for _ in 0..10 {
            if let Ok((tx, rx)) = ipc::connect().await {
                return (tx, rx, true);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    let (events, rx) = mpsc::unbounded_channel();
    let (tx, cmds) = mpsc::unbounded_channel();
    let (_, present) = watch::channel(true);
    host::start(ep.clone(), home.to_path_buf(), store.clone(), net::whoami(), UiLink { events, cmds, present, summon: None });
    (tx, rx, false)
}

#[cfg(windows)]
fn message(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};
    unsafe { MessageBoxW(None, &HSTRING::from(text), w!("Deskline"), MB_OK) };
}

/// `--server`: the host side of the installed service, as SYSTEM in the signed-in session. Its
/// identity and settings live in %ProgramData%\Deskline, so the code is the computer's, not a
/// user's.
#[cfg(windows)]
fn server() -> anyhow::Result<()> {
    let dir = service::data_dir()?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let store = Store::open(&dir);
        let ep = net::start(&dir, store.get().relay.as_deref()).await?;
        let (events, ev_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmds) = mpsc::unbounded_channel();
        let (present_tx, present) = watch::channel(false);
        ipc::serve(ev_rx, cmd_tx, present_tx);
        let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into());
        host::start(ep, dir.clone(), store, name, UiLink { events, cmds, present, summon: Some(|| service::spawn_ui(&["--background"])) });
        update::run_in_service(dir.join("update"));
        std::future::pending::<anyhow::Result<()>>().await
    })
}
