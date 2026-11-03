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

use tokio_util::sync::CancellationToken;

pub mod code;

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
    _private: (),
}

impl PendingSend {
    /// The code to display, e.g. `42-river-candle`.
    pub fn code(&self) -> &str {
        todo!("pairing agent")
    }

    /// Wait for a receiver to join with the code and finish pairing.
    pub async fn wait_for_peer(self, cancel: CancellationToken) -> Result<Paired, PairingError> {
        let _ = cancel;
        todo!("pairing agent")
    }
}

pub async fn allocate(cfg: &Config) -> Result<PendingSend, PairingError> {
    let _ = cfg;
    todo!("pairing agent")
}

/// Receiver side: join with the code typed by the user.
pub async fn join(cfg: &Config, code: &str, cancel: CancellationToken) -> Result<Paired, PairingError> {
    let _ = (cfg, code, cancel);
    todo!("pairing agent")
}
