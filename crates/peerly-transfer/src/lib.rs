//! Peerly transfer engine.
//!
//! Knows nothing about codes, rendezvous or NAT: it receives an already
//! established, already authenticated [`quinn::Connection`] and moves files
//! over it using the protocol in [`peerly_proto::transfer`].
//!
//! OWNER: transfer agent. Public signatures below are the contract used by
//! `peerly-core`; change them only together with `docs/architecture.md`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use peerly_proto::transfer::Manifest;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub enum TransferEvent {
    /// Receiver side: manifest arrived, waiting for the accept decision.
    Offered(Manifest),
    /// Peer accepted (sender) / we accepted (receiver); bytes start flowing.
    Started { total_bytes: u64, total_files: u32 },
    /// Emitted at most every ~100 ms.
    Progress { bytes_done: u64, bytes_total: u64 },
    FileCompleted { path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub files: u32,
    pub bytes: u64,
    pub elapsed: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("the receiver declined the transfer")]
    Rejected,
    #[error("cancelled")]
    Cancelled,
    #[error("peer cancelled: {0}")]
    PeerCancelled(String),
    #[error("integrity check failed for {0}")]
    HashMismatch(String),
    #[error("unsafe path in manifest: {0}")]
    UnsafePath(String),
    #[error("not enough disk space: need {needed} bytes, have {available}")]
    DiskFull { needed: u64, available: u64 },
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("connection lost: {0}")]
    Connection(String),
    #[error(transparent)]
    Codec(#[from] peerly_proto::CodecError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Send the given files and/or folders. Folders are sent recursively, keeping
/// their name as the top-level directory on the receiver side.
pub async fn send(
    conn: &quinn::Connection,
    paths: &[PathBuf],
    events: UnboundedSender<TransferEvent>,
    cancel: CancellationToken,
) -> Result<Summary, TransferError> {
    let _ = (conn, paths, events, cancel);
    todo!("transfer agent")
}

/// Receive into `dest_dir`. `decide` is called once with the manifest; return
/// `true` to accept. Files are written as `<name>.peerly-part` and renamed only
/// after their BLAKE3 hash verifies.
pub async fn receive<F, Fut>(
    conn: &quinn::Connection,
    dest_dir: &Path,
    decide: F,
    events: UnboundedSender<TransferEvent>,
    cancel: CancellationToken,
) -> Result<Summary, TransferError>
where
    F: FnOnce(Manifest) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    let _ = (conn, dest_dir, decide, events, cancel);
    todo!("transfer agent")
}
