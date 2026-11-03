//! Peerly pairing: turns a short human code into an authenticated, encrypted,
//! direct QUIC connection between two peers.
//!
//! Steps (see `docs/architecture.md`):
//! 1. Sender allocates a nameplate on the rendezvous server, generates the code.
//! 2. Receiver claims the nameplate from the code.
//! 3. SPAKE2 over the rendezvous relay, password = full code -> shared key.
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

use seal::{Keys, Role};

pub mod code;
mod net;
pub mod quic;
mod rendezvous;
mod seal;

pub use quic::transport_config;

#[derive(Debug, Clone)]
pub struct Config {
    /// e.g. `ws://127.0.0.1:9009`. The UDP probe uses the same host and port.
    pub rendezvous_url: String,
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
pub struct PendingSend {
    code: String,
    cfg: Config,
    rv: rendezvous::Client,
}

impl std::fmt::Debug for PendingSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSend")
            .field("rendezvous_url", &self.cfg.rendezvous_url)
            .finish_non_exhaustive()
    }
}

impl PendingSend {
    /// The code to display, e.g. `42-river-candle`.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Wait for a receiver to join with the code and finish pairing.
    /// Waiting for the receiver has no timeout (only `cancel` and the server's nameplate TTL).
    pub async fn wait_for_peer(self, cancel: CancellationToken) -> Result<Paired, PairingError> {
        let PendingSend { code, cfg, mut rv } = self;
        cancellable(&cancel, async move {
            let early = rv.wait_peer_joined().await?;
            pair(rv, &code, Role::Sender, &cfg, early).await
        })
        .await
    }
}

pub async fn allocate(cfg: &Config) -> Result<PendingSend, PairingError> {
    let mut rv = rendezvous::Client::connect(&cfg.rendezvous_url).await?;
    let nameplate = rv.allocate().await?;
    let code = code::Code::generate(nameplate).to_string();
    Ok(PendingSend {
        code,
        cfg: cfg.clone(),
        rv,
    })
}

/// Receiver side: join with the code typed by the user.
pub async fn join(
    cfg: &Config,
    code: &str,
    cancel: CancellationToken,
) -> Result<Paired, PairingError> {
    let code = code::Code::parse(code).ok_or(PairingError::InvalidCode)?;
    cancellable(&cancel, async {
        let mut rv = rendezvous::Client::connect(&cfg.rendezvous_url).await?;
        rv.claim(code.nameplate).await?;
        pair(rv, &code.to_string(), Role::Receiver, cfg, None).await
    })
    .await
}

/// SPAKE2 identity (both sides, symmetric mode).
const PAKE_IDENTITY: &[u8] = b"peerly/1";
/// Upper bound for each peer-to-peer exchange step over the relay.
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

async fn next_pairing_msg(
    rv: &mut rendezvous::Client,
    early: &mut Option<Vec<u8>>,
) -> Result<PairingMsg, PairingError> {
    let payload = match early.take() {
        Some(p) => p,
        None => tokio::time::timeout(EXCHANGE_TIMEOUT, rv.next_peer_payload())
            .await
            .map_err(|_| PairingError::Timeout)??,
    };
    peerly_proto::decode(&payload)
        .map_err(|e| PairingError::Protocol(format!("bad pairing message: {e}")))
}

/// Steps 3–6: SPAKE2, sealed `PeerInfo` exchange, then QUIC by role.
async fn pair(
    mut rv: rendezvous::Client,
    password: &str,
    role: Role,
    cfg: &Config,
    mut early: Option<Vec<u8>>,
) -> Result<Paired, PairingError> {
    let (pake, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(password.as_bytes()),
        &Identity::new(PAKE_IDENTITY),
    );
    rv.relay(&PairingMsg::Pake(outbound)).await?;

    // Probe + cert generation run while we wait for the peer's PAKE message.
    let (local, peer_pake) = tokio::join!(net::prepare(&cfg.rendezvous_url), async {
        match next_pairing_msg(&mut rv, &mut early).await? {
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
    rv.relay(&keys.seal(&mine)?).await?;
    let peer = match next_pairing_msg(&mut rv, &mut early).await? {
        PairingMsg::Sealed { nonce, ciphertext } => keys.open(nonce, &ciphertext)?,
        PairingMsg::Pake(_) => {
            return Err(PairingError::Protocol("unexpected PAKE message".into()));
        }
    };
    rv.close().await;
    tracing::debug!(?role, candidates = ?peer.candidates, "peer info exchanged");

    match role {
        Role::Sender => quic::accept(local, peer).await,
        Role::Receiver => quic::connect(local, peer).await,
    }
}
