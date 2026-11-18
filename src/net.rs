//! The endpoint: iroh QUIC dialed by endpoint id (hole punching, relay fallback, mDNS on the
//! LAN) with a persistent identity, so this computer's code never changes.
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use iroh::endpoint::{Connection, QuicTransportConfig, presets};
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_mdns_address_lookup::MdnsAddressLookup;

use crate::proto::ALPN;

/// Bind the endpoint with the identity saved in `home`. `relay`: your own relay server's URL
/// instead of n0's public ones.
pub async fn start(home: &Path, relay: Option<&str>) -> anyhow::Result<Endpoint> {
    // A dead link is noticed in ~12 s instead of 30, so the viewer reconnects sooner.
    let transport = QuicTransportConfig::builder().max_idle_timeout(Some(Duration::from_secs(12).try_into()?)).build();
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(identity(home)?)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport);
    if let Some(url) = relay.map(str::trim).filter(|u| !u.is_empty()) {
        let url = url.parse().map_err(|e| anyhow::anyhow!("relay server address {url:?}: {e}"))?;
        builder = builder.relay_mode(RelayMode::custom([url]));
    }
    let ep = builder.bind().await?;
    let mdns = MdnsAddressLookup::builder().build(ep.id())?;
    ep.address_lookup()?.add(mdns);
    Ok(ep)
}

fn identity(home: &Path) -> anyhow::Result<SecretKey> {
    let path = home.join("identity");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(SecretKey::from_bytes(&bytes));
        }
    }
    let key = SecretKey::generate();
    std::fs::create_dir_all(home)?;
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    file.open(&path)?.write_all(&key.to_bytes())?;
    Ok(key)
}

/// The code people share: z-base-32 of the public key, in groups of four.
pub fn code(id: EndpointId) -> String {
    let z = id.to_z32();
    z.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

/// Accepts the code with or without spaces or dashes.
pub fn parse_code(s: &str) -> Option<EndpointId> {
    let z: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    EndpointId::from_z32(&z).ok()
}

pub fn short(id: EndpointId) -> String {
    id.to_z32()[..8].to_string()
}

/// Round trip time of the path in use.
pub fn rtt(conn: &Connection) -> Duration {
    conn.paths().iter().find(|p| p.is_selected()).map_or(Duration::from_millis(100), |p| p.rtt())
}

/// The path in use: "LAN", "Direct" or "Relay", and its round trip in ms.
pub fn link(conn: &Connection) -> (&'static str, u32) {
    let paths = conn.paths();
    let Some(p) = paths.iter().find(|p| p.is_selected()) else { return ("Relay", 0) };
    let kind = match p.remote_addr() {
        TransportAddr::Ip(a) if is_lan(a.ip()) => "LAN",
        TransportAddr::Ip(_) => "Direct",
        _ => "Relay",
    };
    (kind, p.rtt().as_millis().min(u32::MAX as u128) as u32)
}

fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// "user@host" for the other side's prompt.
pub fn whoami() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let user = var("USER").or_else(|| var("USERNAME")).unwrap_or_else(|| "someone".into());
    let host = var("COMPUTERNAME")
        .or_else(|| var("HOSTNAME"))
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string()))
        .filter(|h| !h.is_empty());
    match host {
        Some(h) => format!("{user}@{h}"),
        None => user,
    }
}
