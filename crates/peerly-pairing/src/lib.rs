//! Peerly pairing: turns a short human code into an authenticated, encrypted,
//! direct QUIC connection between two peers.
//!
//! Steps (see `docs/architecture.md`):
//! 1. Sender gets a nameplate (rendezvous server, or a local random 4-digit one
//!    checked for collisions on the DHT), generates the code and opens its
//!    mailboxes: rendezvous relay, LAN listener advertised over mDNS, DHT slots.
//! 2. Receiver looks for the sender on the LAN (mDNS) and the DHT at the same
//!    time, and claims the nameplate on the rendezvous if configured.
//! 3. SPAKE2 over that mailbox (see `mailbox.rs`), password = full code -> shared key.
//! 4. Each side seals its `PeerInfo` (cert fingerprint + candidate addresses) with that key.
//! 5. Hole punching + QUIC handshake with mutual certificate pinning.
//!
//! OWNER: pairing agent. Public signatures below are the contract used by
//! `peerly-core`; change them only together with `docs/architecture.md`.

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerly_proto::pairing::{PairingMsg, PeerInfo};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio_util::sync::CancellationToken;

use dht::{DhtMailbox, DhtNode};
use lan::LanListener;
use mailbox::Mailbox;
use net::{Discovery, Local};
use quic::AcceptWindow;
use rendezvous::RendezvousMailbox;
use seal::{Keys, Role};

pub mod code;
mod dht;
mod lan;
mod mailbox;
mod mdns;
mod net;
pub mod quic;
mod rendezvous;
mod seal;
mod stun;

#[cfg(test)]
mod dht_tests;
#[cfg(test)]
mod lan_tests;

pub use quic::transport_config;

/// Which pairing channels ("mailboxes") to use. At least one must be enabled.
#[derive(Debug, Clone)]
pub struct Config {
    /// Optional rendezvous server, e.g. `ws://127.0.0.1:9009`. The UDP probe
    /// uses the same host and port.
    pub rendezvous_url: Option<String>,
    /// Serverless pairing on the local network (mDNS + TCP).
    pub lan: bool,
    /// Serverless pairing over the internet (Mainline DHT + STUN).
    pub dht: bool,
}

impl Default for Config {
    /// Serverless: LAN + DHT, no rendezvous server.
    fn default() -> Self {
        Config {
            rendezvous_url: None,
            lan: true,
            dht: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Connected over a local network address.
    Lan,
    /// Connected over the internet (hole punched).
    Internet,
}

/// A ready connection. Sender is the QUIC server, receiver the QUIC client.
pub struct Paired {
    pub connection: quinn::Connection,
    /// Keep alive as long as `connection` is used.
    pub endpoint: quinn::Endpoint,
    pub peer_addr: SocketAddr,
    pub route: Route,
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("cannot reach the rendezvous server: {0}")]
    RendezvousUnreachable(String),
    #[error("this code does not exist or has expired")]
    CodeNotFound,
    #[error("this code is already in use")]
    CodeInUse,
    #[error("wrong code")]
    WrongCode,
    #[error("invalid code format")]
    InvalidCode,
    #[error("could not establish a direct connection to the peer")]
    PeerUnreachable,
    #[error("the other side left")]
    PeerLeft,
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Internal knobs that tests override (production uses [`Env::default`]).
#[derive(Clone)]
pub(crate) struct Env {
    /// Advertise / browse via mDNS (off: LAN tests connect to the listener directly).
    mdns: bool,
    /// DHT bootstrap nodes; `None` = public Mainline DHT.
    dht_bootstrap: Option<Vec<String>>,
    /// STUN servers for the DHT path.
    stun: Vec<String>,
    /// Counts DHT lookups (tests check that polling stops).
    dht_gets: Arc<AtomicUsize>,
    /// Local nameplates to use first, in order (tests force collisions).
    nameplates: Arc<Mutex<Vec<u32>>>,
}

impl Default for Env {
    fn default() -> Self {
        Env {
            mdns: true,
            dht_bootstrap: None,
            stun: stun::DEFAULT_SERVERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            dht_gets: Arc::default(),
            nameplates: Arc::default(),
        }
    }
}

impl Env {
    /// Locally generated nameplate (1000..=9999) for serverless pairing.
    fn next_nameplate(&self) -> u32 {
        if let Ok(mut forced) = self.nameplates.lock()
            && !forced.is_empty()
        {
            return forced.remove(0);
        }
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).expect("OS random number generator unavailable");
        u32::from_le_bytes(b) % 9000 + 1000
    }

    fn dht_node(&self) -> Result<DhtNode, PairingError> {
        DhtNode::start(self.dht_bootstrap.as_deref(), self.dht_gets.clone())
    }
}

/// Sender side, step 1: allocate a nameplate and generate the code to show.
///
/// Holds the open mailboxes (rendezvous connection, LAN listener + mDNS
/// advertisement, DHT session). Dropping it tears all of them down.
pub struct PendingSend {
    code: String,
    rendezvous: Option<(rendezvous::Client, String)>,
    lan: Option<LanListener>,
    dht: Option<DhtMailbox>,
    stun: Vec<String>,
}

impl std::fmt::Debug for PendingSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSend")
            .field("rendezvous_url", &self.rendezvous.as_ref().map(|(_, u)| u))
            .field("lan", &self.lan.is_some())
            .field("dht", &self.dht.is_some())
            .finish_non_exhaustive()
    }
}

/// Which mailbox completed the exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Rendezvous,
    Lan,
    Dht,
}

impl PendingSend {
    /// The code to display, e.g. `4217-river-candle`.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Wait for a receiver to join with the code and finish pairing.
    /// All mailboxes are raced; the first completed exchange wins and the
    /// others (LAN listener, mDNS advertisement, DHT polling, rendezvous
    /// connection) are dropped. Waiting for the receiver has no timeout (only
    /// `cancel` and the server's nameplate TTL).
    pub async fn wait_for_peer(self, cancel: CancellationToken) -> Result<Paired, PairingError> {
        cancellable(&cancel, async move {
            let PendingSend {
                code,
                rendezvous,
                lan,
                dht,
                stun,
            } = self;
            let (local, peer, via) = sender_exchange(&code, rendezvous, lan, dht, &stun).await?;
            let window = match via {
                // The receiver reads our sealed info up to one poll step later.
                Via::Dht => AcceptWindow::lagging_receiver(EXCHANGE_TIMEOUT),
                Via::Lan | Via::Rendezvous => AcceptWindow::DIRECT,
            };
            let paired = quic::accept(local, peer, window).await?;
            Ok(with_route(paired, via))
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn lan_port(&self) -> Option<u16> {
        self.lan.as_ref().map(LanListener::port)
    }

    #[cfg(test)]
    pub(crate) fn has_dht(&self) -> bool {
        self.dht.is_some()
    }
}

/// LAN pairing always means a direct local route, whatever the address range.
fn with_route(mut paired: Paired, via: Via) -> Paired {
    if via == Via::Lan {
        paired.route = Route::Lan;
    }
    paired
}

/// Races the sender's mailboxes. Every mailbox is dropped on return.
async fn sender_exchange(
    code: &str,
    rendezvous: Option<(rendezvous::Client, String)>,
    lan: Option<LanListener>,
    dht: Option<DhtMailbox>,
    stun: &[String],
) -> Result<(Local, PeerInfo, Via), PairingError> {
    let mut rv_done = rendezvous.is_none();
    let mut lan_done = lan.is_none();
    let mut dht_done = dht.is_none();
    let rv_fut = async move {
        let Some((mut client, url)) = rendezvous else {
            return Err(PairingError::RendezvousUnreachable("not configured".into()));
        };
        let early = client.wait_peer_joined().await?;
        let mut mb = RendezvousMailbox::new(client, early);
        let r = exchange(&mut mb, code, Role::Sender, Discovery::Rendezvous(&url)).await;
        mb.close().await;
        r
    };
    let lan_fut = async move {
        match lan {
            Some(l) => l.serve(code).await,
            None => Err(PairingError::CodeNotFound),
        }
    };
    let dht_fut = async move {
        let Some(mut mb) = dht else {
            return Err(PairingError::CodeNotFound);
        };
        exchange(&mut mb, code, Role::Sender, Discovery::Stun(stun)).await
    };
    tokio::pin!(rv_fut, lan_fut, dht_fut);
    let mut last_err = None;
    while !(rv_done && lan_done && dht_done) {
        let (via, r) = tokio::select! {
            r = &mut rv_fut, if !rv_done => { rv_done = true; (Via::Rendezvous, r) }
            r = &mut lan_fut, if !lan_done => { lan_done = true; (Via::Lan, r) }
            r = &mut dht_fut, if !dht_done => { dht_done = true; (Via::Dht, r) }
        };
        match r {
            Ok((local, peer)) => return Ok((local, peer, via)),
            // A wrong code ends pairing everywhere (single use on the rendezvous
            // and the DHT, too many guesses on the LAN).
            Err(PairingError::WrongCode) => return Err(PairingError::WrongCode),
            Err(e) => {
                if !(rv_done && lan_done && dht_done) {
                    tracing::warn!(
                        ?via,
                        "pairing channel failed, still waiting on the others: {e}"
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(PairingError::PeerLeft))
}

/// Sender: opens every configured mailbox and generates the code.
///
/// The nameplate comes from the rendezvous server when it is configured and
/// reachable; otherwise (with `lan` or `dht`) it is generated locally and,
/// with `dht`, re-picked while another sender's fresh session occupies it.
pub async fn allocate(cfg: &Config) -> Result<PendingSend, PairingError> {
    allocate_with(cfg, &Env::default()).await
}

async fn allocate_with(cfg: &Config, env: &Env) -> Result<PendingSend, PairingError> {
    let serverless = cfg.lan || cfg.dht;
    let mut rendezvous = None;
    let mut assigned = None;
    if let Some(url) = &cfg.rendezvous_url {
        let r = async {
            let mut c = rendezvous::Client::connect(url).await?;
            let np = c.allocate().await?;
            Ok::<_, PairingError>((c, np))
        }
        .await;
        match r {
            Ok((c, np)) => {
                rendezvous = Some((c, url.clone()));
                assigned = Some(np);
            }
            Err(e) if serverless => {
                tracing::warn!("rendezvous unavailable, pairing without a server: {e}");
            }
            Err(e) => return Err(e),
        }
    } else if !serverless {
        return Err(PairingError::RendezvousUnreachable(
            "no rendezvous server configured and serverless pairing is disabled".into(),
        ));
    }

    let mut dht_node = None;
    if cfg.dht {
        match env.dht_node() {
            Ok(n) => dht_node = Some(n),
            Err(e) => tracing::warn!("DHT pairing unavailable: {e}"),
        }
    }
    let nameplate = match assigned {
        Some(np) => {
            if let Some(node) = &dht_node
                && node.nameplate_taken(np).await
            {
                tracing::warn!(%np, "DHT: nameplate busy, pairing without the DHT");
                dht_node = None;
            }
            np
        }
        None => {
            let mut np = env.next_nameplate();
            if let Some(node) = &dht_node {
                let mut tries = 1;
                while node.nameplate_taken(np).await {
                    if tries >= dht::MAX_NAMEPLATE_TRIES {
                        tracing::warn!("DHT: no free nameplate found, pairing without the DHT");
                        dht_node = None;
                        break;
                    }
                    tracing::debug!(%np, "DHT: nameplate busy, picking another");
                    np = env.next_nameplate();
                    tries += 1;
                }
            }
            np
        }
    };
    let dht = dht_node.map(|node| DhtMailbox::offer(node, nameplate));

    let lan = if cfg.lan {
        match LanListener::bind(nameplate, env.mdns).await {
            Ok(l) => Some(l),
            Err(e) if rendezvous.is_some() || dht.is_some() => {
                tracing::warn!("LAN pairing unavailable: {e}");
                None
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    if rendezvous.is_none() && lan.is_none() && dht.is_none() {
        // Only reachable when the DHT was the sole channel and had no free nameplate.
        return Err(if cfg.dht {
            PairingError::CodeInUse
        } else {
            PairingError::RendezvousUnreachable("no pairing channel available".into())
        });
    }
    Ok(PendingSend {
        code: code::Code::generate(nameplate).to_string(),
        rendezvous,
        lan,
        dht,
        stun: env.stun.clone(),
    })
}

/// Receiver side: join with the code typed by the user.
///
/// LAN (mDNS, ~2 s) and DHT lookups run concurrently; the rendezvous server,
/// if configured, is tried once the LAN browse found nothing. Preference when
/// several succeed: LAN, then rendezvous, then DHT.
pub async fn join(
    cfg: &Config,
    code: &str,
    cancel: CancellationToken,
) -> Result<Paired, PairingError> {
    join_with(cfg, code, cancel, &Env::default()).await
}

/// Ranks errors by how useful they are to the user.
fn error_rank(e: &PairingError) -> u8 {
    match e {
        PairingError::WrongCode => 3,
        PairingError::CodeNotFound => 1,
        _ => 2,
    }
}

async fn join_with(
    cfg: &Config,
    code: &str,
    cancel: CancellationToken,
    env: &Env,
) -> Result<Paired, PairingError> {
    let code = code::Code::parse(code).ok_or(PairingError::InvalidCode)?;
    let password = code.to_string();
    cancellable(&cancel, async {
        // The rendezvous claim waits for the LAN browse: claiming burns a
        // rendezvous nameplate, and a LAN sender would pair twice otherwise.
        let (lan_gate_tx, lan_gate_rx) = tokio::sync::oneshot::channel::<()>();
        let mut lan_gate = Some(lan_gate_tx);
        if !cfg.lan {
            lan_gate = None;
        }
        let lan_fut = async {
            if cfg.lan && env.mdns {
                lan::discover_and_exchange(&code).await
            } else {
                Err(PairingError::CodeNotFound)
            }
        };
        let rv_fut = async {
            let _ = lan_gate_rx.await;
            match &cfg.rendezvous_url {
                Some(url) => rendezvous_exchange(url, &code).await,
                None => Err(PairingError::CodeNotFound),
            }
        };
        let dht_fut = async {
            let node = env.dht_node()?;
            let mut mb = DhtMailbox::join(node, code.nameplate).await?;
            exchange(
                &mut mb,
                &password,
                Role::Receiver,
                Discovery::Stun(&env.stun),
            )
            .await
        };
        tokio::pin!(lan_fut, rv_fut, dht_fut);
        let mut lan_done = !cfg.lan;
        let mut rv_done = cfg.rendezvous_url.is_none();
        let mut dht_done = !cfg.dht;
        let mut held_dht = None;
        let mut err = PairingError::CodeNotFound;
        loop {
            if lan_done && rv_done {
                if let Some((local, peer)) = held_dht.take() {
                    return connect_as_receiver(local, peer, Via::Dht).await;
                }
                if dht_done {
                    return Err(err);
                }
            }
            let (via, r) = tokio::select! {
                r = &mut lan_fut, if !lan_done => {
                    lan_done = true;
                    drop(lan_gate.take());
                    (Via::Lan, r)
                }
                r = &mut rv_fut, if !rv_done => { rv_done = true; (Via::Rendezvous, r) }
                r = &mut dht_fut, if !dht_done => { dht_done = true; (Via::Dht, r) }
            };
            match r {
                Ok((local, peer)) if via == Via::Dht => held_dht = Some((local, peer)),
                Ok((local, peer)) => return connect_as_receiver(local, peer, via).await,
                Err(e) => {
                    tracing::debug!(?via, "receiver pairing channel failed: {e}");
                    if error_rank(&e) > error_rank(&err) {
                        err = e;
                    }
                }
            }
        }
    })
    .await
}

async fn rendezvous_exchange(
    url: &str,
    code: &code::Code,
) -> Result<(Local, PeerInfo), PairingError> {
    let mut client = rendezvous::Client::connect(url).await?;
    client.claim(code.nameplate).await?;
    let mut mb = RendezvousMailbox::new(client, None);
    let r = exchange(
        &mut mb,
        &code.to_string(),
        Role::Receiver,
        Discovery::Rendezvous(url),
    )
    .await;
    mb.close().await;
    r
}

async fn connect_as_receiver(
    local: Local,
    peer: PeerInfo,
    via: Via,
) -> Result<Paired, PairingError> {
    let paired = quic::connect(local, peer).await?;
    Ok(with_route(paired, via))
}

/// SPAKE2 identity (both sides, symmetric mode).
const PAKE_IDENTITY: &[u8] = b"peerly/1";
/// Upper bound for each peer-to-peer exchange step over a mailbox.
pub(crate) const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

async fn cancellable<T>(
    cancel: &CancellationToken,
    fut: impl Future<Output = Result<T, PairingError>>,
) -> Result<T, PairingError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(PairingError::Cancelled),
        r = fut => r,
    }
}

async fn next_msg<M: Mailbox>(mb: &mut M) -> Result<PairingMsg, PairingError> {
    tokio::time::timeout(EXCHANGE_TIMEOUT, mb.recv())
        .await
        .map_err(|_| PairingError::Timeout)?
}

fn expect_pake(msg: PairingMsg) -> Result<Vec<u8>, PairingError> {
    match msg {
        PairingMsg::Pake(m) => Ok(m),
        other => Err(PairingError::Protocol(format!(
            "expected PAKE message, got {other:?}"
        ))),
    }
}

/// Steps 3-4 over any mailbox: SPAKE2, then the sealed `PeerInfo` exchange.
/// `discovery` = how to learn our reflexive address on the QUIC socket.
/// The caller closes the mailbox and runs QUIC by role.
pub(crate) async fn exchange<M: Mailbox>(
    mb: &mut M,
    password: &str,
    role: Role,
    discovery: Discovery<'_>,
) -> Result<(Local, PeerInfo), PairingError> {
    let (pake, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(password.as_bytes()),
        &Identity::new(PAKE_IDENTITY),
    );
    mb.send(&PairingMsg::Pake(outbound)).await?;

    let (local, peer_pake) = if mb.waits_for_peer() {
        // Unbounded wait for the peer; prepare afterwards so the NAT mapping
        // learned via STUN is fresh when QUIC starts.
        let peer_pake = expect_pake(mb.recv().await?)?;
        (net::prepare(discovery).await?, peer_pake)
    } else {
        // Probe + cert generation run while we wait for the peer's PAKE message.
        let (local, peer_pake) = tokio::join!(net::prepare(discovery), async {
            expect_pake(next_msg(mb).await?)
        });
        (local?, peer_pake?)
    };
    let shared = pake
        .finish(&peer_pake)
        .map_err(|e| PairingError::Protocol(format!("PAKE failed: {e:?}")))?;

    let keys = Keys::derive(&shared, role);
    let mine = PeerInfo {
        cert_fingerprint: local.identity.fingerprint,
        candidates: local.candidates.clone(),
    };
    mb.send(&keys.seal(&mine)?).await?;
    let peer = match next_msg(mb).await? {
        PairingMsg::Sealed { nonce, ciphertext } => keys.open(nonce, &ciphertext)?,
        PairingMsg::Pake(_) => {
            return Err(PairingError::Protocol("unexpected PAKE message".into()));
        }
    };
    tracing::debug!(?role, candidates = ?peer.candidates, "peer info exchanged");
    Ok((local, peer))
}
