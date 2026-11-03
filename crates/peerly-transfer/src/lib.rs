//! Peerly transfer engine.
//!
//! Knows nothing about codes, rendezvous or NAT: it receives an already
//! established, already authenticated [`quinn::Connection`] and moves files
//! over it using the protocol in [`peerly_proto::transfer`].
//!
//! OWNER: transfer agent. Public signatures below are the contract used by
//! `peerly-core`; change them only together with `docs/architecture.md`.

mod manifest;
mod paths;
mod recv;
mod send;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// their name as the top-level directory on the receiver side. Symlinks are
/// skipped (never followed). Closes `conn` when finished.
pub async fn send(
    conn: &quinn::Connection,
    paths: &[PathBuf],
    events: UnboundedSender<TransferEvent>,
    cancel: CancellationToken,
) -> Result<Summary, TransferError> {
    send::send(conn, paths, events, cancel).await
}

/// Receive into `dest_dir`. `decide` is called once with the manifest; return
/// `true` to accept. Files are written as `<name>.peerly-part` and renamed only
/// after their BLAKE3 hash verifies. Existing files are never overwritten: a
/// colliding top-level item is renamed `name (1).ext` / `folder (1)`.
/// Returns [`TransferError::Rejected`] if `decide` returned `false`.
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
    recv::receive(conn, dest_dir, decide, events, cancel).await
}

// ---- shared internals -------------------------------------------------------

/// Concurrent data streams (one file each).
pub(crate) const MAX_PARALLEL_STREAMS: usize = 6;
/// Disk I/O buffer size.
pub(crate) const BUF_SIZE: usize = 1024 * 1024;
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// QUIC application close codes.
pub(crate) const CLOSE_DONE: u32 = 0;
pub(crate) const CLOSE_CANCEL: u32 = 1;
pub(crate) const CLOSE_ERROR: u32 = 2;

pub(crate) fn conn_err(e: impl std::fmt::Display) -> TransferError {
    TransferError::Connection(e.to_string())
}

pub(crate) fn join_err(e: tokio::task::JoinError) -> TransferError {
    TransferError::Io(std::io::Error::other(e))
}

/// Short name of a control message for error messages (avoids dumping manifests).
pub(crate) fn kind(c: &peerly_proto::transfer::Control) -> &'static str {
    use peerly_proto::transfer::Control::*;
    match c {
        Offer(_) => "Offer",
        Accept => "Accept",
        Reject => "Reject",
        Done => "Done",
        Cancel { .. } => "Cancel",
    }
}

/// Turn a low-level failure into `PeerCancelled` / `Connection` when the
/// connection has been closed underneath us.
pub(crate) fn classify(conn: &quinn::Connection, err: TransferError) -> TransferError {
    use TransferError::*;
    if !matches!(err, Codec(_) | Io(_) | Connection(_) | Protocol(_)) {
        return err;
    }
    match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(c))
            if c.error_code != quinn::VarInt::from_u32(CLOSE_DONE) =>
        {
            PeerCancelled(String::from_utf8_lossy(&c.reason).into_owned())
        }
        None | Some(quinn::ConnectionError::LocallyClosed) => err,
        Some(e) => Connection(e.to_string()),
    }
}

/// Tell the peer why we stop (best effort) and close the connection.
pub(crate) async fn abort(
    conn: &quinn::Connection,
    ctrl: Option<&mut quinn::SendStream>,
    err: &TransferError,
) {
    use TransferError::*;
    let (code, mut reason) = match err {
        Rejected => (CLOSE_DONE, "rejected".to_owned()),
        Cancelled => (CLOSE_CANCEL, "cancelled".to_owned()),
        PeerCancelled(_) | Connection(_) => (CLOSE_ERROR, String::new()),
        e => (CLOSE_ERROR, e.to_string()),
    };
    if code != CLOSE_DONE
        && !reason.is_empty()
        && let Some(tx) = ctrl
    {
        // Give the frame a short chance to be acknowledged before the close
        // discards it; the close reason carries the same text as a fallback.
        let msg = peerly_proto::transfer::Control::Cancel { reason: reason.clone() };
        let deliver = async {
            if peerly_proto::frame::write_frame(tx, &msg).await.is_ok() && tx.finish().is_ok() {
                let _ = tx.stopped().await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_millis(500), deliver).await;
    }
    let mut cut = reason.len().min(512);
    while !reason.is_char_boundary(cut) {
        cut -= 1;
    }
    reason.truncate(cut);
    conn.close(code.into(), reason.as_bytes());
}

/// Emits throttled `Progress` events from a shared byte counter.
pub(crate) struct ProgressTicker {
    interval: tokio::time::Interval,
    total: u64,
    last: Option<u64>,
}

impl ProgressTicker {
    pub(crate) fn new(total: u64) -> Self {
        let mut interval = tokio::time::interval(PROGRESS_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self { interval, total, last: None }
    }

    pub(crate) async fn tick(&mut self) {
        self.interval.tick().await;
    }

    /// Emit if the counter moved (or `force`).
    pub(crate) fn emit(
        &mut self,
        done: &AtomicU64,
        events: &UnboundedSender<TransferEvent>,
        force: bool,
    ) {
        let d = done.load(Ordering::Relaxed);
        if force || self.last != Some(d) {
            self.last = Some(d);
            let _ = events.send(TransferEvent::Progress { bytes_done: d, bytes_total: self.total });
        }
    }
}
