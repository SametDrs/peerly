//! Peerly high-level API. The only crate the CLI and the desktop app talk to.
//! Glues `peerly-pairing` (code -> connection) and `peerly-transfer` (files over the connection),
//! and turns their events into one UI-friendly [`Event`] stream (serde-serializable for Tauri).
//!
//! OWNER: integration agent (phase 2). Signatures are the contract for the CLI and the desktop app.

use std::future::Future;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub use peerly_pairing::Route;
pub use peerly_proto::transfer::Manifest;

pub const DEFAULT_RENDEZVOUS: &str = "ws://127.0.0.1:9009";

#[derive(Debug, Clone)]
pub struct Config {
    pub rendezvous_url: String,
}

impl Config {
    /// `PEERLY_RENDEZVOUS` env var, else [`DEFAULT_RENDEZVOUS`].
    pub fn from_env() -> Self {
        Self {
            rendezvous_url: std::env::var("PEERLY_RENDEZVOUS")
                .unwrap_or_else(|_| DEFAULT_RENDEZVOUS.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// Sender: show this code to the user.
    Code { code: String },
    WaitingForPeer,
    Connecting,
    Connected { route: &'static str },
    /// Receiver: offer received, waiting for the accept decision.
    Offer { name: String, files: u32, total_bytes: u64 },
    Started { total_bytes: u64, total_files: u32 },
    Progress { bytes_done: u64, bytes_total: u64, bytes_per_sec: u64 },
    FileCompleted { path: String },
    Completed { files: u32, bytes: u64, elapsed_ms: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Pairing(#[from] peerly_pairing::PairingError),
    #[error(transparent)]
    Transfer(#[from] peerly_transfer::TransferError),
}

pub async fn send(
    cfg: &Config,
    paths: Vec<PathBuf>,
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<(), Error> {
    let _ = (cfg, paths, events, cancel);
    todo!("integration agent")
}

pub async fn receive<F, Fut>(
    cfg: &Config,
    code: &str,
    dest_dir: &Path,
    decide: F,
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<(), Error>
where
    F: FnOnce(Manifest) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    let _ = (cfg, code, dest_dir, decide, events, cancel);
    todo!("integration agent")
}
