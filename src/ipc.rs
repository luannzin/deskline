//! Installed mode on Windows: the SYSTEM `--server` owns the host side, and the signed-in user's
//! window talks to it over a named pipe. Only SYSTEM, administrators and interactive users can
//! open the pipe; the window checks the other end is elevated (not a squatter), and the server
//! takes changes to who gets in without a click only from an elevated window.
use std::os::windows::io::AsRawHandle;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions};
use tokio::sync::{mpsc, watch};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows::core::w;

use crate::host::{HostCmd, HostEvent};
use crate::proto::{recv_msg, send_msg};
use crate::service;

fn pipe_name() -> String {
    format!(r"\\.\pipe\deskline-{}", service::session_id())
}

/// Server: bridge the host's channels to the window connected right now (one at a time).
/// While no window is connected events are dropped; a window asks for the state on connect.
pub fn serve(mut events: mpsc::UnboundedReceiver<HostEvent>, cmds: mpsc::UnboundedSender<HostCmd>, present: watch::Sender<bool>) {
    tokio::spawn(async move {
        let name = pipe_name();
        let mut first = true;
        loop {
            let server = match create(&name, first) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("deskline: pipe {name}: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            };
            first = false;
            let connected = async {
                loop {
                    tokio::select! {
                        r = server.connect() => return r,
                        e = events.recv() => if e.is_none() { return Err(std::io::Error::other("host gone")) },
                    }
                }
            };
            if connected.await.is_err() {
                if cmds.is_closed() {
                    return;
                }
                continue;
            }
            let mut pid = 0;
            let elevated = unsafe { GetNamedPipeClientProcessId(HANDLE(server.as_raw_handle()), &mut pid) }.is_ok() && service::process_is_elevated(pid);
            present.send_replace(true);
            let (mut r, mut w) = tokio::io::split(server);
            let to_host = cmds.clone();
            let mut reader = tokio::spawn(async move {
                while let Ok(c) = recv_msg::<HostCmd, _>(&mut r).await {
                    if c.is_admin() && !elevated {
                        continue;
                    }
                    if to_host.send(c).is_err() {
                        break;
                    }
                }
            });
            loop {
                tokio::select! {
                    e = events.recv() => match e {
                        Some(e) => if send_msg(&mut w, &e).await.is_err() { break },
                        None => return,
                    },
                    _ = &mut reader => break,
                }
            }
            reader.abort();
            present.send_replace(false);
        }
    });
}

fn create(name: &str, first: bool) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    // SYSTEM and administrators: full access; interactive users: read and write.
    let sd = service::descriptor(w!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"))?;
    let mut sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: (*sd).0, bInheritHandle: false.into() };
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut sa).cast())
    };
    Ok(server?)
}

fn open() -> anyhow::Result<NamedPipeClient> {
    let client = ClientOptions::new().open(pipe_name())?;
    let mut pid = 0;
    unsafe { GetNamedPipeServerProcessId(HANDLE(client.as_raw_handle()), &mut pid)? };
    anyhow::ensure!(service::process_is_elevated(pid), "the Deskline pipe isn't served by the Deskline service");
    Ok(client)
}

/// Window: connect to this session's server (errors when it isn't there). Events arrive on the
/// receiver, commands go out on the sender. If the server restarts (an update), this reconnects
/// and asks for the state again; commands sent meanwhile are dropped.
pub async fn connect() -> anyhow::Result<(mpsc::UnboundedSender<HostCmd>, mpsc::UnboundedReceiver<HostEvent>)> {
    let mut client = Some(open()?);
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HostCmd>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let c = match client.take() {
                Some(c) => c,
                None => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if ev_tx.is_closed() {
                        return;
                    }
                    match open() {
                        Ok(c) => c,
                        Err(_) => continue,
                    }
                }
            };
            let (mut r, mut w) = tokio::io::split(c);
            let events = ev_tx.clone();
            // Reading on its own task: a read must never be cancelled halfway through a message.
            let mut reader = tokio::spawn(async move {
                while let Ok(e) = recv_msg::<HostEvent, _>(&mut r).await {
                    if events.send(e).is_err() {
                        break;
                    }
                }
            });
            if send_msg(&mut w, &HostCmd::Hello).await.is_ok() {
                loop {
                    tokio::select! {
                        c = cmd_rx.recv() => match c {
                            Some(c) => if send_msg(&mut w, &c).await.is_err() { break },
                            None => return,
                        },
                        _ = &mut reader => break,
                    }
                }
            }
            reader.abort();
        }
    });
    Ok((cmd_tx, ev_rx))
}

