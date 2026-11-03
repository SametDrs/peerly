//! Peerly pairing: turns a short human code into an authenticated, encrypted,
//! direct QUIC connection between two peers.
//!
//! Steps (see `docs/architecture.md`):
//! 1. Sender gets a nameplate (rendezvous server, or locally for LAN-only), generates the code,
//!    and opens its mailboxes: rendezvous relay and/or a LAN listener advertised over mDNS.
//! 2. Receiver finds the sender: mDNS on the LAN first, then claims the nameplate on the rendezvous.
//! 3. SPAKE2 over that mailbox (see `mailbox.rs`), password = full code -> shared key.
//! 4. Each side seals its `PeerInfo` (cert fingerprint + candidate addresses) with that key.
//! 5. Hole punching + QUIC handshake with mutual certificate pinning.
//!
//! OWNER: pairing agent. Public signatures below are the contract used by
//! `peerly-core`; change them only together with `docs/architecture.md`.

use std::net::SocketAddr;
use std::time::Duration;

use peerly_proto::pairing::{PairingMsg, PeerInfo};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio_util::sync::CancellationToken;

use lan::LanListener;
use mailbox::Mailbox;
use net::Local;
use rendezvous::RendezvousMailbox;
use seal::{Keys, Role};

pub mod code;
mod lan;
mod mailbox;
mod mdns;
mod net;
pub mod quic;
mod rendezvous;
mod seal;

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

/// Sender side, step 1: allocate a nameplate and generate the code to show.
///
/// Holds the open mailboxes (rendezvous connection and/or LAN listener +
/// mDNS advertisement). Dropping it tears all of them down.
pub struct PendingSend {
    code: String,
    rendezvous: Option<(rendezvous::Client, String)>,
    lan: Option<LanListener>,
}

impl std::fmt::Debug for PendingSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSend")
            .field("rendezvous_url", &self.rendezvous.as_ref().map(|(_, u)| u))
            .field("lan", &self.lan.is_some())
            .finish_non_exhaustive()
    }
}

/// Which mailbox completed the exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Rendezvous,
    Lan,
}

impl PendingSend {
    /// The code to display, e.g. `42-river-candle`.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Wait for a receiver to join with the code and finish pairing.
    /// All mailboxes are raced; the first completed exchange wins and the
    /// others (including the LAN listener and mDNS advertisement) are dropped.
    /// Waiting for the receiver has no timeout (only `cancel` and the server's nameplate TTL).
    pub async fn wait_for_peer(self, cancel: CancellationToken) -> Result<Paired, PairingError> {
        cancellable(&cancel, async move {
            let PendingSend {
                code,
                rendezvous,
                lan,
            } = self;
            let (local, peer, via) = sender_exchange(&code, rendezvous, lan).await?;
            let paired = quic::accept(local, peer).await?;
            Ok(with_route(paired, via))
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn lan_port(&self) -> Option<u16> {
        self.lan.as_ref().map(LanListener::port)
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
) -> Result<(Local, PeerInfo, Via), PairingError> {
    let mut rv_done = rendezvous.is_none();
    let mut lan_done = lan.is_none();
    let rv_fut = async move {
        let Some((mut client, url)) = rendezvous else {
            return Err(PairingError::RendezvousUnreachable("not configured".into()));
        };
        let early = client.wait_peer_joined().await?;
        let mut mb = RendezvousMailbox::new(client, early);
        let r = exchange(&mut mb, code, Role::Sender, Some(&url)).await;
        mb.close().await;
        r
    };
    let lan_fut = async move {
        match lan {
            Some(l) => l.serve(code).await,
            None => Err(PairingError::CodeNotFound),
        }
    };
    tokio::pin!(rv_fut, lan_fut);
    let mut last_err = None;
    while !(rv_done && lan_done) {
        let (via, r) = tokio::select! {
            r = &mut rv_fut, if !rv_done => { rv_done = true; (Via::Rendezvous, r) }
            r = &mut lan_fut, if !lan_done => { lan_done = true; (Via::Lan, r) }
        };
        match r {
            Ok((local, peer)) => return Ok((local, peer, via)),
            // A wrong code ends pairing everywhere (single use on the rendezvous,
            // too many guesses on the LAN).
            Err(PairingError::WrongCode) => return Err(PairingError::WrongCode),
            Err(e) => {
                if !(rv_done && lan_done) {
                    tracing::warn!(
                        ?via,
                        "pairing channel failed, still waiting on the other: {e}"
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(PairingError::PeerLeft))
}

/// Locally generated nameplate (1..=999) for serverless pairing.
fn local_nameplate() -> u32 {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS random number generator unavailable");
    u32::from_le_bytes(b) % 999 + 1
}

/// Sender: opens every configured mailbox and generates the code.
///
/// The nameplate comes from the rendezvous server when it is configured and
/// reachable; otherwise (with `lan`) it is generated locally.
pub async fn allocate(cfg: &Config) -> Result<PendingSend, PairingError> {
    allocate_with(cfg, true).await
}

/// `advertise = false` skips mDNS (tests connect to the listener directly).
async fn allocate_with(cfg: &Config, advertise: bool) -> Result<PendingSend, PairingError> {
    let mut rendezvous = None;
    let nameplate = match &cfg.rendezvous_url {
        Some(url) => {
            let r = async {
                let mut c = rendezvous::Client::connect(url).await?;
                let np = c.allocate().await?;
                Ok::<_, PairingError>((c, np))
            }
            .await;
            match r {
                Ok((c, np)) => {
                    rendezvous = Some((c, url.clone()));
                    np
                }
                Err(e) if cfg.lan => {
                    tracing::warn!(
                        "rendezvous unavailable, pairing on the local network only: {e}"
                    );
                    local_nameplate()
                }
                Err(e) => return Err(e),
            }
        }
        None if cfg.lan => local_nameplate(),
        None => {
            return Err(PairingError::RendezvousUnreachable(
                "no rendezvous server configured and LAN pairing is disabled".into(),
            ));
        }
    };
    let lan = if cfg.lan {
        match LanListener::bind(nameplate, advertise).await {
            Ok(l) => Some(l),
            Err(e) if rendezvous.is_some() => {
                tracing::warn!("LAN pairing unavailable: {e}");
                None
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    Ok(PendingSend {
        code: code::Code::generate(nameplate).to_string(),
        rendezvous,
        lan,
    })
}

/// Receiver side: join with the code typed by the user.
///
/// With `lan`, first browses the local network (~2 s) and tries every sender
/// advertising the nameplate; then falls back to the rendezvous server if configured.
pub async fn join(
    cfg: &Config,
    code: &str,
    cancel: CancellationToken,
) -> Result<Paired, PairingError> {
    let code = code::Code::parse(code).ok_or(PairingError::InvalidCode)?;
    cancellable(&cancel, async {
        let mut lan_err = None;
        if cfg.lan {
            match lan::discover_and_exchange(&code).await {
                Ok((local, peer)) => return connect_as_receiver(local, peer, Via::Lan).await,
                Err(PairingError::CodeNotFound) => {}
                Err(e) => lan_err = Some(e),
            }
        }
        let Some(url) = &cfg.rendezvous_url else {
            return Err(lan_err.unwrap_or(PairingError::CodeNotFound));
        };
        match join_rendezvous(url, &code).await {
            // The LAN failure (e.g. a wrong code) says more than "not found".
            Err(PairingError::CodeNotFound) if lan_err.is_some() => {
                Err(lan_err.unwrap_or(PairingError::CodeNotFound))
            }
            r => r,
        }
    })
    .await
}

async fn join_rendezvous(url: &str, code: &code::Code) -> Result<Paired, PairingError> {
    let mut client = rendezvous::Client::connect(url).await?;
    client.claim(code.nameplate).await?;
    let mut mb = RendezvousMailbox::new(client, None);
    let r = exchange(&mut mb, &code.to_string(), Role::Receiver, Some(url)).await;
    mb.close().await;
    let (local, peer) = r?;
    connect_as_receiver(local, peer, Via::Rendezvous).await
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
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Steps 3-4 over any mailbox: SPAKE2, then the sealed `PeerInfo` exchange.
/// `probe_url` = rendezvous server to probe for a reflexive address (none on the LAN).
/// The caller closes the mailbox and runs QUIC by role.
pub(crate) async fn exchange<M: Mailbox>(
    mb: &mut M,
    password: &str,
    role: Role,
    probe_url: Option<&str>,
) -> Result<(Local, PeerInfo), PairingError> {
    let (pake, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(password.as_bytes()),
        &Identity::new(PAKE_IDENTITY),
    );
    mb.send(&PairingMsg::Pake(outbound)).await?;

    // Probe + cert generation run while we wait for the peer's PAKE message.
    let (local, peer_pake) = tokio::join!(net::prepare(probe_url), async {
        match next_msg(mb).await? {
            PairingMsg::Pake(m) => Ok(m),
            other => Err(PairingError::Protocol(format!(
                "expected PAKE message, got {other:?}"
            ))),
        }
    });
    let (local, peer_pake) = (local?, peer_pake?);
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
