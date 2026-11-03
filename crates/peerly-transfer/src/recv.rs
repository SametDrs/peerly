//! Receiver side.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerly_proto::frame::{read_frame, write_frame};
use peerly_proto::transfer::{Control, Manifest, StreamHeader};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::paths::{self, part_path};
use crate::{
    BUF_SIZE, ProgressTicker, Summary, TransferError, TransferEvent, abort, classify, conn_err,
    join_err, kind,
};

type Parts = Arc<Mutex<HashSet<PathBuf>>>;

pub(crate) async fn receive<F, Fut>(
    conn: &quinn::Connection,
    dest: &Path,
    decide: F,
    events: UnboundedSender<TransferEvent>,
    cancel: CancellationToken,
) -> Result<Summary, TransferError>
where
    F: FnOnce(Manifest) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    let mut ctrl = None;
    let mut workers = JoinSet::new();
    let parts: Parts = Arc::default();
    let res = run(conn, dest, decide, &events, &cancel, &mut ctrl, &mut workers, &parts).await;
    match res {
        Ok(s) => Ok(s),
        Err(e) => {
            let e = classify(conn, e);
            abort(conn, ctrl.as_mut(), &e).await;
            // Stop every writer before deleting its partial file.
            workers.shutdown().await;
            let leftovers: Vec<PathBuf> = parts.lock().unwrap().drain().collect();
            for p in leftovers {
                if tokio::fs::remove_file(&p).await.is_err() {
                    // A just-aborted blocking write may still hold the handle.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tokio::fs::remove_file(&p).await;
                }
            }
            Err(e)
        }
    }
}

struct Shared {
    manifest: Arc<Manifest>,
    targets: Vec<PathBuf>,
    seen: Vec<AtomicBool>,
    parts: Parts,
    bytes: AtomicU64,
    events: UnboundedSender<TransferEvent>,
}

fn unexpected(r: Result<Control, peerly_proto::CodecError>) -> TransferError {
    match r {
        Ok(Control::Cancel { reason }) => TransferError::PeerCancelled(reason),
        Ok(other) => TransferError::Protocol(format!("unexpected {}", kind(&other))),
        Err(e) => e.into(),
    }
}

/// Finish our control stream and give the peer a moment to read it.
async fn finish_ctrl(conn: &quinn::Connection, tx: &mut quinn::SendStream, msg: Control) {
    if write_frame(tx, &msg).await.is_err() || tx.finish().is_err() {
        return;
    }
    tokio::select! {
        _ = tx.stopped() => {}
        _ = conn.closed() => {}
        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn run<F, Fut>(
    conn: &quinn::Connection,
    dest: &Path,
    decide: F,
    events: &UnboundedSender<TransferEvent>,
    cancel: &CancellationToken,
    ctrl: &mut Option<quinn::SendStream>,
    workers: &mut JoinSet<Result<(), TransferError>>,
    parts: &Parts,
) -> Result<Summary, TransferError>
where
    F: FnOnce(Manifest) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    let (tx, mut rx) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TransferError::Cancelled),
        r = conn.accept_bi() => r.map_err(conn_err)?,
    };
    let tx = ctrl.insert(tx);
    let first = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TransferError::Cancelled),
        r = read_frame::<_, Control>(&mut rx) => r,
    };
    let manifest = match first {
        Ok(Control::Offer(m)) => m,
        other => return Err(unexpected(other)),
    };
    paths::validate_manifest(&manifest)?;

    let dest_owned = dest.to_path_buf();
    let available = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dest_owned)?;
        fs4::available_space(&dest_owned)
    })
    .await
    .map_err(join_err)??;
    if available < manifest.total_size {
        return Err(TransferError::DiskFull { needed: manifest.total_size, available });
    }

    let _ = events.send(TransferEvent::Offered(manifest.clone()));
    let mut ctrl_fut = Box::pin(read_frame::<_, Control>(&mut rx));
    let accepted = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TransferError::Cancelled),
        r = &mut ctrl_fut => return Err(unexpected(r)),
        a = decide(manifest.clone()) => a,
    };
    if !accepted {
        finish_ctrl(conn, tx, Control::Reject).await;
        return Err(TransferError::Rejected);
    }

    // Plan after the decision: the user may have taken a while and the
    // destination may have changed meanwhile.
    let manifest = Arc::new(manifest);
    let (m, dest_owned) = (manifest.clone(), dest.to_path_buf());
    let plan = tokio::task::spawn_blocking(move || {
        let plan = paths::plan(&dest_owned, &m);
        paths::create_dirs(&dest_owned, &plan).map(|_| plan)
    })
    .await
    .map_err(join_err)??;

    write_frame(tx, &Control::Accept).await?;
    let start = Instant::now();
    let n = manifest.files.len();
    let total = manifest.total_size;
    let _ = events.send(TransferEvent::Started { total_bytes: total, total_files: n as u32 });
    let shared = Arc::new(Shared {
        seen: (0..n).map(|_| AtomicBool::new(false)).collect(),
        manifest,
        targets: plan.files,
        parts: parts.clone(),
        bytes: AtomicU64::new(0),
        events: events.clone(),
    });

    let mut ticker = ProgressTicker::new(total);
    let (mut opened, mut completed) = (0usize, 0usize);
    while completed < n {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            r = &mut ctrl_fut => return Err(unexpected(r)),
            Some(r) = workers.join_next(), if !workers.is_empty() => match r.map_err(join_err)? {
                Ok(()) => completed += 1,
                // A peer that aborts resets its data streams before its `Cancel`
                // frame / close arrives: give those a moment to explain why.
                Err(e @ TransferError::Connection(_)) => {
                    let grace = Duration::from_secs(1);
                    return Err(match tokio::time::timeout(grace, &mut ctrl_fut).await {
                        Ok(r) => unexpected(r),
                        Err(_) => e,
                    });
                }
                Err(e) => return Err(e),
            },
            s = conn.accept_uni(), if opened < n => {
                let s = s.map_err(conn_err)?;
                opened += 1;
                workers.spawn(recv_file(s, shared.clone()));
            }
            _ = ticker.tick() => ticker.emit(&shared.bytes, events, false),
        }
    }
    ticker.emit(&shared.bytes, events, true);
    finish_ctrl(conn, tx, Control::Done).await;
    Ok(Summary { files: n as u32, bytes: total, elapsed: start.elapsed() })
}

async fn recv_file(mut s: quinn::RecvStream, sh: Arc<Shared>) -> Result<(), TransferError> {
    let hdr: StreamHeader = read_frame(&mut s).await?;
    let idx = hdr.index as usize;
    let entry = sh
        .manifest
        .files
        .get(idx)
        .ok_or_else(|| TransferError::Protocol(format!("unknown file index {idx}")))?;
    if hdr.offset != 0 {
        return Err(TransferError::Protocol(format!("unsupported offset {}", hdr.offset)));
    }
    if sh.seen[idx].swap(true, Ordering::Relaxed) {
        return Err(TransferError::Protocol(format!("duplicate stream for index {idx}")));
    }
    let target = &sh.targets[idx];
    let part = part_path(target);
    sh.parts.lock().unwrap().insert(part.clone());
    let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&part).await?;

    // Network chunks are small (~1 packet); batch them into one large buffer so
    // both hashing (SIMD-friendly) and disk writes work on big blocks. tokio's
    // `File::write_all` copies the block and writes it in the background, which
    // overlaps disk I/O with receiving the next block.
    let mut hasher = blake3::Hasher::new();
    let mut buf: Vec<u8> = Vec::with_capacity(BUF_SIZE.min(entry.size as usize));
    let mut remaining = entry.size;
    while remaining > 0 {
        let max = usize::try_from(remaining).unwrap_or(usize::MAX);
        let chunk = s.read_chunk(max, true).await.map_err(conn_err)?.ok_or_else(|| {
            TransferError::Protocol(format!("{}: stream ended early", entry.path))
        })?;
        let len = chunk.bytes.len();
        remaining -= len as u64;
        sh.bytes.fetch_add(len as u64, Ordering::Relaxed);
        if buf.len() + len > buf.capacity() && !buf.is_empty() {
            hasher.update(&buf);
            file.write_all(&buf).await?;
            buf.clear();
        }
        if len > buf.capacity() {
            hasher.update(&chunk.bytes);
            file.write_all(&chunk.bytes).await?;
        } else {
            buf.extend_from_slice(&chunk.bytes);
        }
    }
    if !buf.is_empty() {
        hasher.update(&buf);
        file.write_all(&buf).await?;
    }
    let mut trailer = [0u8; 32];
    s.read_exact(&mut trailer).await.map_err(|e| match e {
        quinn::ReadExactError::FinishedEarly(_) => {
            TransferError::Protocol(format!("{}: missing hash trailer", entry.path))
        }
        quinn::ReadExactError::ReadError(e) => conn_err(e),
    })?;
    if s.read_chunk(1, true).await.map_err(conn_err)?.is_some() {
        return Err(TransferError::Protocol(format!("{}: more bytes than announced", entry.path)));
    }
    file.flush().await?;
    drop(file);
    if hasher.finalize() != trailer {
        return Err(TransferError::HashMismatch(entry.path.clone()));
    }
    tokio::fs::rename(&part, target).await?;
    sh.parts.lock().unwrap().remove(&part);
    let _ = sh.events.send(TransferEvent::FileCompleted { path: entry.path.clone() });
    Ok(())
}
