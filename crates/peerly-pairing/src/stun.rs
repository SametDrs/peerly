//! Minimal STUN (RFC 5389) Binding client: learns our public IPv4 address and
//! port as seen by a public STUN server, on the socket that later becomes the
//! QUIC endpoint. Only what we need: a Binding request without attributes and
//! the (XOR-)MAPPED-ADDRESS of a success response.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;

/// Public STUN servers queried in parallel when pairing over the DHT.
pub(crate) const DEFAULT_SERVERS: &[&str] =
    &["stun.l.google.com:19302", "stun.cloudflare.com:3478"];
/// Total budget for DNS + requests; without an answer we continue with LAN candidates.
const BUDGET: Duration = Duration::from_secs(2);
const RESEND: Duration = Duration::from_millis(500);

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const FAMILY_IPV4: u8 = 0x01;
const HEADER_LEN: usize = 20;

/// A Binding request: 20-byte header, no attributes.
pub(crate) fn binding_request(txid: &[u8; 12]) -> [u8; HEADER_LEN] {
    let mut b = [0u8; HEADER_LEN];
    b[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    // b[2..4]: message length 0.
    b[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    b[8..20].copy_from_slice(txid);
    b
}

/// Parses a Binding success response for `txid`. Returns the IPv4 mapped
/// address (XOR-MAPPED-ADDRESS preferred over MAPPED-ADDRESS).
pub(crate) fn parse_binding_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < HEADER_LEN
        || u16::from_be_bytes([buf[0], buf[1]]) != BINDING_SUCCESS
        || u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC_COOKIE
        || &buf[8..20] != txid
    {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
    let body = buf.get(HEADER_LEN..HEADER_LEN + len)?;
    let mut mapped = None;
    let mut i = 0;
    while i + 4 <= body.len() {
        let ty = u16::from_be_bytes([body[i], body[i + 1]]);
        let alen = usize::from(u16::from_be_bytes([body[i + 2], body[i + 3]]));
        let value = body.get(i + 4..i + 4 + alen)?;
        match ty {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some((port, ip)) = ipv4_attr(value) {
                    let port = port ^ (MAGIC_COOKIE >> 16) as u16;
                    let ip = ip ^ MAGIC_COOKIE;
                    return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port));
                }
            }
            ATTR_MAPPED_ADDRESS => {
                if let Some((port, ip)) = ipv4_attr(value) {
                    mapped = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port));
                }
            }
            _ => {}
        }
        // Attributes are padded to a multiple of 4 bytes.
        i += 4 + alen.div_ceil(4) * 4;
    }
    mapped
}

/// `(port, ipv4)` from a (XOR-)MAPPED-ADDRESS value: reserved, family, port, address.
fn ipv4_attr(v: &[u8]) -> Option<(u16, u32)> {
    if v.len() < 8 || v[1] != FAMILY_IPV4 {
        return None;
    }
    Some((
        u16::from_be_bytes([v[2], v[3]]),
        u32::from_be_bytes([v[4], v[5], v[6], v[7]]),
    ))
}

/// Asks all `servers` in parallel; first valid answer wins. `None` if none
/// answered within the budget.
pub(crate) async fn reflexive(sock: &UdpSocket, servers: &[String]) -> Option<SocketAddr> {
    if servers.is_empty() {
        return None;
    }
    tokio::time::timeout(BUDGET, async {
        let mut targets = Vec::new();
        for s in servers {
            match tokio::net::lookup_host(s.as_str()).await {
                Ok(addrs) => targets.extend(addrs.filter(SocketAddr::is_ipv4).take(1)),
                Err(e) => tracing::debug!(server = %s, "STUN lookup failed: {e}"),
            }
        }
        if targets.is_empty() {
            return None;
        }
        let mut txid = [0u8; 12];
        getrandom::fill(&mut txid).ok()?;
        let req = binding_request(&txid);
        let mut buf = [0u8; 576];
        loop {
            for t in &targets {
                let _ = sock.send_to(&req, t).await;
            }
            let deadline = tokio::time::Instant::now() + RESEND;
            while let Ok(res) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
                // Errors (e.g. ICMP-induced resets on Windows) just mean "keep waiting".
                if let Ok((n, from)) = res
                    && targets.contains(&from)
                    && let Some(addr) = parse_binding_response(&buf[..n], &txid)
                {
                    tracing::debug!(%from, %addr, "STUN answer");
                    return Some(addr);
                }
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5769, 2.2 "Sample IPv4 Response" (192.0.2.1:32853).
    const RFC5769_IPV4_RESPONSE: [u8; 80] = [
        0x01, 0x01, 0x00, 0x3c, 0x21, 0x12, 0xa4, 0x42, 0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6,
        0x86, 0xfa, 0x87, 0xdf, 0xae, 0x80, 0x22, 0x00, 0x0b, 0x74, 0x65, 0x73, 0x74, 0x20, 0x76,
        0x65, 0x63, 0x74, 0x6f, 0x72, 0x20, 0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1,
        0x12, 0xa6, 0x43, 0x00, 0x08, 0x00, 0x14, 0x2b, 0x91, 0xf5, 0x99, 0xfd, 0x9e, 0x90, 0xc3,
        0x8c, 0x74, 0x89, 0xf9, 0x2a, 0xf9, 0xba, 0x53, 0xf0, 0x6b, 0xe7, 0xd7, 0x80, 0x28, 0x00,
        0x04, 0xc0, 0x7d, 0x4c, 0x96,
    ];
    const RFC5769_TXID: [u8; 12] = [
        0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
    ];

    #[test]
    fn parses_rfc5769_ipv4_response() {
        let addr = parse_binding_response(&RFC5769_IPV4_RESPONSE, &RFC5769_TXID).unwrap();
        assert_eq!(addr, "192.0.2.1:32853".parse().unwrap());
    }

    #[test]
    fn rejects_wrong_txid_type_and_truncation() {
        let mut other = RFC5769_TXID;
        other[0] ^= 1;
        assert_eq!(parse_binding_response(&RFC5769_IPV4_RESPONSE, &other), None);
        let mut req = RFC5769_IPV4_RESPONSE;
        req[1] = 0x11; // error response
        assert_eq!(parse_binding_response(&req, &RFC5769_TXID), None);
        assert_eq!(
            parse_binding_response(&RFC5769_IPV4_RESPONSE[..50], &RFC5769_TXID),
            None
        );
    }

    #[test]
    fn falls_back_to_mapped_address() {
        let txid = [7u8; 12];
        let mut b = binding_request(&txid).to_vec();
        b[0..2].copy_from_slice(&BINDING_SUCCESS.to_be_bytes());
        b[2..4].copy_from_slice(&12u16.to_be_bytes());
        b.extend_from_slice(&[0x00, 0x01, 0x00, 0x08, 0x00, 0x01, 0x1f, 0x90, 10, 0, 0, 7]);
        assert_eq!(
            parse_binding_response(&b, &txid),
            Some("10.0.0.7:8080".parse().unwrap())
        );
    }

    #[test]
    fn request_layout() {
        let txid = [9u8; 12];
        let r = binding_request(&txid);
        assert_eq!(&r[0..8], &[0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(&r[8..], &txid);
    }

    /// Real public STUN servers (needs internet).
    #[tokio::test]
    #[ignore = "needs internet (public STUN)"]
    async fn real_stun_answers() {
        let sock = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let servers: Vec<String> = DEFAULT_SERVERS.iter().map(|s| s.to_string()).collect();
        let t = std::time::Instant::now();
        let addr = reflexive(&sock, &servers).await;
        println!("STUN: {addr:?} in {:?}", t.elapsed());
        assert!(addr.is_some());
    }
}
