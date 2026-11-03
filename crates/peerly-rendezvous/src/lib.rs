//! Peerly rendezvous server library: WebSocket pairing + UDP address probe.
//! Never logs or inspects relay payloads.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use peerly_proto::probe::{MAGIC, ProbeRequest, ProbeResponse};
use peerly_proto::rendezvous::{ClientMsg, ErrorCode, NAMEPLATE_TTL_SECS, ServerMsg};
use peerly_proto::{PROTOCOL_VERSION, decode, encode};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

pub const MAX_WS_MESSAGE: usize = 64 * 1024;
pub const MAX_RELAY_PAYLOAD: usize = 8 * 1024;
pub const MAX_RELAYS_PER_NAMEPLATE: u32 = 32;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    /// Unclaimed nameplates expire after this long.
    pub nameplate_ttl: Duration,
    /// Max Allocate/Claim requests per IP per minute.
    pub rate_limit_per_min: u32,
    pub idle_timeout: Duration,
    pub max_nameplates: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9009".parse().unwrap(),
            nameplate_ttl: Duration::from_secs(NAMEPLATE_TTL_SECS),
            rate_limit_per_min: 30,
            idle_timeout: Duration::from_secs(15 * 60),
            max_nameplates: 100_000,
        }
    }
}

type Tx = mpsc::UnboundedSender<Message>;

struct Member {
    id: u64,
    tx: Tx,
}

struct Plate {
    owner: Member,
    joiner: Option<Member>,
    created: Instant,
    relays: u32,
}

#[derive(Default)]
struct Inner {
    plates: HashMap<u32, Plate>,
    by_conn: HashMap<u64, u32>,
    rate: HashMap<IpAddr, (Instant, u32)>,
}

struct State {
    cfg: ServerConfig,
    inner: Mutex<Inner>,
    next_id: AtomicU64,
}

fn send(tx: &Tx, m: &ServerMsg) {
    if let Ok(s) = serde_json::to_string(m) {
        let _ = tx.send(Message::Text(s.into()));
    }
}

fn err(tx: &Tx, code: ErrorCode, message: &str) {
    send(
        tx,
        &ServerMsg::Error {
            code,
            message: message.into(),
        },
    );
}

fn rand_u64() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

impl State {
    fn rate_ok(&self, inner: &mut Inner, ip: IpAddr) -> bool {
        let now = Instant::now();
        if inner.rate.len() > 50_000 {
            inner
                .rate
                .retain(|_, (s, _)| now.duration_since(*s) < Duration::from_secs(60));
        }
        let e = inner.rate.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(60) {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= self.cfg.rate_limit_per_min
    }

    fn expire(&self, inner: &mut Inner) {
        let ttl = self.cfg.nameplate_ttl;
        let dead: Vec<u32> = inner
            .plates
            .iter()
            .filter(|(_, p)| p.joiner.is_none() && p.created.elapsed() >= ttl)
            .map(|(n, _)| *n)
            .collect();
        for n in dead {
            if let Some(p) = inner.plates.remove(&n) {
                inner.by_conn.remove(&p.owner.id);
                err(
                    &p.owner.tx,
                    ErrorCode::NameplateNotFound,
                    "nameplate expired",
                );
            }
        }
    }

    /// Short numbers first: 1..=99 while <50% used, then 1..=999, then up to 999_999.
    fn pick_nameplate(&self, inner: &Inner) -> Option<u32> {
        for hi in [99u32, 999, 999_999] {
            if hi != 999_999 {
                let in_range = inner.plates.keys().filter(|n| **n <= hi).count();
                if in_range * 2 >= hi as usize {
                    continue;
                }
            }
            for _ in 0..200 {
                let n = 1 + (rand_u64() % hi as u64) as u32;
                if !inner.plates.contains_key(&n) {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Remove the connection's nameplate (if any), telling the other side.
    fn leave(&self, id: u64) {
        let mut g = self.inner.lock().unwrap();
        let Some(n) = g.by_conn.remove(&id) else {
            return;
        };
        if let Some(p) = g.plates.remove(&n) {
            let other = if p.owner.id == id {
                p.joiner
            } else {
                Some(p.owner)
            };
            if let Some(o) = other {
                g.by_conn.remove(&o.id);
                send(&o.tx, &ServerMsg::PeerLeft);
            }
        }
    }

    /// Returns false when the connection should close.
    fn handle(&self, id: u64, ip: IpAddr, tx: &Tx, msg: ClientMsg) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let g = &mut *guard;
        match msg {
            ClientMsg::Hello { .. } => err(tx, ErrorCode::BadRequest, "already greeted"),
            ClientMsg::Allocate | ClientMsg::Claim { .. } => {
                if !self.rate_ok(g, ip) {
                    err(tx, ErrorCode::RateLimited, "too many requests");
                    return true;
                }
                self.expire(g);
                if g.by_conn.contains_key(&id) {
                    err(tx, ErrorCode::BadRequest, "already holding a nameplate");
                    return true;
                }
                if let ClientMsg::Claim { nameplate } = msg {
                    let Some(p) = g.plates.get_mut(&nameplate) else {
                        err(tx, ErrorCode::NameplateNotFound, "no such nameplate");
                        return true;
                    };
                    if p.joiner.is_some() {
                        err(tx, ErrorCode::NameplateFull, "nameplate full");
                        return true;
                    }
                    send(&p.owner.tx, &ServerMsg::PeerJoined);
                    p.joiner = Some(Member { id, tx: tx.clone() });
                    g.by_conn.insert(id, nameplate);
                    send(tx, &ServerMsg::Claimed);
                } else {
                    if g.plates.len() >= self.cfg.max_nameplates {
                        err(tx, ErrorCode::RateLimited, "server full");
                        return true;
                    }
                    let Some(n) = self.pick_nameplate(g) else {
                        err(tx, ErrorCode::RateLimited, "server full");
                        return true;
                    };
                    g.plates.insert(
                        n,
                        Plate {
                            owner: Member { id, tx: tx.clone() },
                            joiner: None,
                            created: Instant::now(),
                            relays: 0,
                        },
                    );
                    g.by_conn.insert(id, n);
                    send(tx, &ServerMsg::Allocated { nameplate: n });
                }
            }
            ClientMsg::Relay { payload } => {
                if payload.len() > MAX_RELAY_PAYLOAD {
                    err(tx, ErrorCode::BadRequest, "payload too large");
                    return true;
                }
                let Some(p) = g.by_conn.get(&id).and_then(|n| g.plates.get_mut(n)) else {
                    err(tx, ErrorCode::NoPeer, "no peer");
                    return true;
                };
                let other = if p.owner.id == id {
                    p.joiner.as_ref()
                } else {
                    Some(&p.owner)
                };
                let Some(other) = other else {
                    err(tx, ErrorCode::NoPeer, "no peer");
                    return true;
                };
                if p.relays >= MAX_RELAYS_PER_NAMEPLATE {
                    err(tx, ErrorCode::RateLimited, "relay limit reached");
                    return true;
                }
                p.relays += 1;
                send(&other.tx, &ServerMsg::Peer { payload });
            }
            ClientMsg::Close => return false,
        }
        true
    }
}

async fn handle_conn(state: Arc<State>, stream: TcpStream, peer: SocketAddr) {
    let wsc = WebSocketConfig::default()
        .max_message_size(Some(MAX_WS_MESSAGE))
        .max_frame_size(Some(MAX_WS_MESSAGE));
    let ws = match tokio_tungstenite::accept_async_with_config(stream, Some(wsc)).await {
        Ok(w) => w,
        Err(e) => {
            tracing::debug!(%peer, "ws handshake failed: {e}");
            return;
        }
    };
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let mut greeted = false;
    loop {
        let t = if greeted {
            state.cfg.idle_timeout
        } else {
            Duration::from_secs(30)
        };
        let m = match tokio::time::timeout(t, stream.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        match m {
            Message::Text(t) => {
                let Ok(msg) = serde_json::from_str::<ClientMsg>(t.as_str()) else {
                    err(&tx, ErrorCode::BadRequest, "malformed message");
                    break;
                };
                if !greeted {
                    match msg {
                        ClientMsg::Hello { version } if version == PROTOCOL_VERSION => {
                            greeted = true;
                            send(
                                &tx,
                                &ServerMsg::Welcome {
                                    version: PROTOCOL_VERSION,
                                },
                            );
                        }
                        ClientMsg::Hello { .. } => {
                            err(
                                &tx,
                                ErrorCode::VersionMismatch,
                                "unsupported protocol version",
                            );
                            break;
                        }
                        _ => {
                            err(&tx, ErrorCode::BadRequest, "expected hello");
                            break;
                        }
                    }
                    continue;
                }
                if !state.handle(id, peer.ip(), &tx, msg) {
                    break;
                }
            }
            Message::Ping(p) => {
                let _ = tx.send(Message::Pong(p));
            }
            Message::Close(_) => break,
            Message::Pong(_) => {}
            _ => {
                err(&tx, ErrorCode::BadRequest, "text frames only");
                break;
            }
        }
    }
    state.leave(id);
    let _ = tx.send(Message::Close(None));
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

async fn udp_loop(sock: UdpSocket) {
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, src)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        let Ok(req) = decode::<ProbeRequest>(&buf[..n]) else {
            continue;
        };
        if req.magic != MAGIC {
            continue;
        }
        if let Ok(out) = encode(&ProbeResponse {
            nonce: req.nonce,
            observed: src,
        }) {
            let _ = sock.send_to(&out, src).await;
        }
    }
}

/// A bound server, ready to run.
pub struct Server {
    tcp: TcpListener,
    udp: UdpSocket,
    cfg: ServerConfig,
    addr: SocketAddr,
}

impl Server {
    /// Bind TCP and UDP on the same port (port 0 picks a free one).
    pub async fn bind(cfg: ServerConfig) -> anyhow::Result<Self> {
        let mut last = None;
        for _ in 0..20 {
            let tcp = TcpListener::bind(cfg.listen).await?;
            let addr = tcp.local_addr()?;
            match UdpSocket::bind(addr).await {
                Ok(udp) => {
                    return Ok(Self {
                        tcp,
                        udp,
                        cfg,
                        addr,
                    });
                }
                Err(e) if cfg.listen.port() == 0 => last = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        Err(anyhow::anyhow!("could not bind UDP on TCP port: {last:?}"))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!(addr = %self.addr, "rendezvous listening (tcp ws + udp probe)");
        let state = Arc::new(State {
            cfg: self.cfg,
            inner: Mutex::new(Inner::default()),
            next_id: AtomicU64::new(1),
        });
        tokio::spawn(udp_loop(self.udp));
        let sweep = state.clone();
        tokio::spawn(async move {
            let every = (sweep.cfg.nameplate_ttl / 4)
                .clamp(Duration::from_millis(20), Duration::from_secs(30));
            loop {
                tokio::time::sleep(every).await;
                let mut g = sweep.inner.lock().unwrap();
                sweep.expire(&mut g);
            }
        });
        loop {
            let (s, peer) = match self.tcp.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            tokio::spawn(handle_conn(state.clone(), s, peer));
        }
    }
}

pub async fn run(cfg: ServerConfig) -> anyhow::Result<()> {
    Server::bind(cfg).await?.run().await
}
