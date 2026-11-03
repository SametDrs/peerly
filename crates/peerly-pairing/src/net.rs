//! Local address discovery: UDP socket, reflexive address (rendezvous probe or
//! STUN), LAN candidates.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use peerly_proto::probe::{MAGIC, ProbeRequest, ProbeResponse};
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::http::Uri;

use crate::PairingError;
use crate::quic::Identity;

const PROBE_ATTEMPTS: u32 = 4;
const PROBE_WAIT: Duration = Duration::from_millis(500);
/// Upper bound on candidates we send or accept.
pub(crate) const MAX_CANDIDATES: usize = 16;

/// Everything one side needs before sealing its `PeerInfo`.
pub(crate) struct Local {
    /// Same socket used for the probe; handed to quinn later.
    pub socket: std::net::UdpSocket,
    pub candidates: Vec<SocketAddr>,
    pub identity: Identity,
}

/// How a side learns its public (reflexive) address, on the QUIC socket itself.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Discovery<'a> {
    /// LAN pairing: local interface addresses only.
    LanOnly,
    /// Rendezvous UDP probe on the server's WebSocket host and port.
    Rendezvous(&'a str),
    /// Public STUN servers (DHT pairing).
    Stun(&'a [String]),
}

/// Binds the QUIC socket and collects candidates: LAN addresses plus the
/// reflexive address found via `discovery` (if any).
pub(crate) async fn prepare(discovery: Discovery<'_>) -> Result<Local, PairingError> {
    let std_sock = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    std_sock.set_nonblocking(true)?;
    let sock = UdpSocket::from_std(std_sock)?;
    let reflexive = match discovery {
        Discovery::LanOnly => None,
        Discovery::Rendezvous(url) => {
            let r = match probe_target(url).await {
                Some(server) => probe(&sock, server).await,
                None => None,
            };
            if r.is_none() {
                tracing::warn!("no UDP probe answer from rendezvous; using LAN candidates only");
            }
            r
        }
        Discovery::Stun(servers) => {
            let r = crate::stun::reflexive(&sock, servers).await.map(normalize);
            if r.is_none() && !servers.is_empty() {
                tracing::warn!("no STUN answer; using LAN candidates only");
            }
            r
        }
    };
    let socket = sock.into_std()?;
    let port = socket.local_addr()?.port();
    let mut candidates = lan_addrs(port);
    if candidates.is_empty() && reflexive.is_none() {
        // No usable interface: at least allow peers on the same machine.
        candidates.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    if let Some(r) = reflexive
        && !candidates.contains(&r)
    {
        candidates.push(r);
    }
    candidates.truncate(MAX_CANDIDATES);
    let identity = Identity::generate()?;
    Ok(Local {
        socket,
        candidates,
        identity,
    })
}

/// UDP probe target: same host and port as the WebSocket URL (IPv4 only).
async fn probe_target(url: &str) -> Option<SocketAddr> {
    let uri: Uri = url.parse().ok()?;
    let host = uri
        .host()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let addrs = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .ok()?
    .ok()?;
    addrs.into_iter().find(SocketAddr::is_ipv4)
}

async fn probe(sock: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).ok()?;
    let req = peerly_proto::encode(&ProbeRequest {
        magic: MAGIC,
        nonce,
    })
    .ok()?;
    let mut buf = [0u8; 512];
    for _ in 0..PROBE_ATTEMPTS {
        if sock.send_to(&req, server).await.is_err() {
            return None;
        }
        let deadline = tokio::time::Instant::now() + PROBE_WAIT;
        while let Ok(res) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
            // Errors (e.g. ICMP-induced resets on Windows) just mean "keep waiting".
            if let Ok((n, _)) = res
                && let Ok(resp) = peerly_proto::decode::<ProbeResponse>(&buf[..n])
                && resp.nonce == nonce
            {
                return Some(normalize(resp.observed));
            }
        }
    }
    None
}

/// Maps IPv4-mapped IPv6 addresses back to plain IPv4.
pub(crate) fn normalize(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), addr.port()),
            None => addr,
        },
        IpAddr::V4(_) => addr,
    }
}

fn lan_addrs(port: u16) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let IpAddr::V4(v4) = iface.ip()
            && !v4.is_loopback()
            && !v4.is_link_local()
            && !v4.is_unspecified()
        {
            let a = SocketAddr::new(IpAddr::V4(v4), port);
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// True for loopback, private, link-local and unique-local addresses.
pub(crate) fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Usable peer candidates: IPv4 (our socket is IPv4), no unspecified/port 0,
/// deduplicated, capped, LAN addresses first.
pub(crate) fn sanitize(candidates: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for a in candidates.into_iter().map(normalize) {
        let IpAddr::V4(ip) = a.ip() else { continue };
        if !ip.is_unspecified()
            && !ip.is_multicast()
            && !ip.is_broadcast()
            && a.port() != 0
            && !out.contains(&a)
        {
            out.push(a);
        }
    }
    out.truncate(MAX_CANDIDATES);
    out.sort_by_key(|a| !is_lan(a.ip()));
    out
}
