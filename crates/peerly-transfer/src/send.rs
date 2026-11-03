//! Sender side.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use peerly_proto::frame::{read_frame, write_frame};
use peerly_proto::transfer::{Control, FileEntry, StreamHeader};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::{
    BUF_SIZE, CLOSE_CANCEL, CLOSE_DONE, MAX_PARALLEL_STREAMS, ProgressTicker, Summary, TransferError,
    TransferEvent, abort, classify, conn_err, join_err, kind, manifest, paths,
};

pub(crate) async fn send(
    conn: &quinn::Connection,
    paths: &[PathBuf],
    events: UnboundedSender<TransferEvent>,
    cancel: CancellationToken,
) -> Result<Summary, TransferError> {
    let mut ctrl = None;
    let mut workers = JoinSet::new();
    let stop = CancellationToken::new();
    match run(conn, paths, &events, &cancel, &mut ctrl, &mut workers, &stop).await {
        Ok(s) => Ok(s),
        Err(e) => {
            // Reset in-flight data streams first: this drops their buffered
            // bytes so the `Cancel` frame is not stuck behind them.
            stop.cancel();
            let drain = async { while workers.join_next().await.is_some() {} };
            let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
            workers.abort_all();
            let e = classify(conn, e);
            abort(conn, ctrl.as_mut(), &e).await;
            Err(e)
        }
    }
}

struct Shared {
    conn: quinn::Connection,
    files: Vec<FileEntry>,
    sources: Vec<PathBuf>,
    next: AtomicUsize,
    bytes: AtomicU64,
    events: UnboundedSender<TransferEvent>,
}

async fn run(
    conn: &quinn::Connection,
    paths: &[PathBuf],
    events: &UnboundedSender<TransferEvent>,
    cancel: &CancellationToken,
    ctrl: &mut Option<quinn::SendStream>,
    workers: &mut JoinSet<Result<(), TransferError>>,
    stop: &CancellationToken,
) -> Result<Summary, TransferError> {
    let owned = paths.to_vec();
    let build = tokio::task::spawn_blocking(move || manifest::build(&owned));
    let (manifest, sources) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TransferError::Cancelled),
        r = build => r.map_err(join_err)??,
    };
    // Fail early (with a clear error) on names the receiver would refuse.
    paths::validate_manifest(&manifest)?;

    let (tx, mut rx) = conn.open_bi().await.map_err(conn_err)?;
    let tx = ctrl.insert(tx);
    // Control frames (e.g. `Cancel`) must overtake queued file data.
    tx.set_priority(1).map_err(conn_err)?;
    write_frame(tx, &Control::Offer(manifest.clone())).await?;
    let reply = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(TransferError::Cancelled),
        r = read_frame::<_, Control>(&mut rx) => r?,
    };
    match reply {
        Control::Accept => {}
        Control::Reject => return Err(TransferError::Rejected),
        Control::Cancel { reason } => return Err(TransferError::PeerCancelled(reason)),
        other => return Err(TransferError::Protocol(format!("unexpected {}", kind(&other)))),
    }

    let start = Instant::now();
    let total = manifest.total_size;
    let n = manifest.files.len();
    let _ = events.send(TransferEvent::Started { total_bytes: total, total_files: n as u32 });
    let shared = Arc::new(Shared {
        conn: conn.clone(),
        files: manifest.files,
        sources,
        next: AtomicUsize::new(0),
        bytes: AtomicU64::new(0),
        events: events.clone(),
    });
    for _ in 0..n.min(MAX_PARALLEL_STREAMS) {
        workers.spawn(worker(shared.clone(), stop.clone()));
    }

    let mut ctrl_fut = Box::pin(read_frame::<_, Control>(&mut rx));
    let mut ticker = ProgressTicker::new(total);
    let mut peer_done = false;
    while !(peer_done && workers.is_empty()) {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TransferError::Cancelled),
            r = &mut ctrl_fut, if !peer_done => match r? {
                Control::Done => peer_done = true,
                Control::Cancel { reason } => return Err(TransferError::PeerCancelled(reason)),
                other => {
                    return Err(TransferError::Protocol(format!("unexpected {}", kind(&other))));
                }
            },
            Some(r) = workers.join_next(), if !workers.is_empty() => r.map_err(join_err)??,
            _ = ticker.tick() => ticker.emit(&shared.bytes, events, false),
        }
    }
    ticker.emit(&shared.bytes, events, true);
    conn.close(CLOSE_DONE.into(), b"done");
    Ok(Summary { files: n as u32, bytes: total, elapsed: start.elapsed() })
}

async fn worker(s: Arc<Shared>, stop: CancellationToken) -> Result<(), TransferError> {
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let i = s.next.fetch_add(1, Ordering::Relaxed);
        if i >= s.files.len() {
            return Ok(());
        }
        let mut stream = None;
        let r = tokio::select! {
            biased;
            _ = stop.cancelled() => Err(TransferError::Cancelled),
            r = send_file(&s, i, &mut stream, &mut buf) => r,
        };
        if r.is_err() {
            // Dropping an unfinished quinn stream would *finish* it; reset instead.
            if let Some(st) = stream.as_mut() {
                let _ = st.reset(CLOSE_CANCEL.into());
            }
            return r;
        }
    }
}

/// One uni stream: `frame(StreamHeader)` + raw bytes + BLAKE3 trailer.
/// The file is read once; hashing happens on the bytes as they are sent.
async fn send_file(
    s: &Shared,
    i: usize,
    slot: &mut Option<quinn::SendStream>,
    buf: &mut [u8],
) -> Result<(), TransferError> {
    let entry = &s.files[i];
    let mut file = tokio::fs::File::open(&s.sources[i]).await?;
    let stream = slot.insert(s.conn.open_uni().await.map_err(conn_err)?);
    write_frame(stream, &StreamHeader { index: entry.index, offset: 0 }).await?;
    let mut hasher = blake3::Hasher::new();
    let mut remaining = entry.size;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(TransferError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("{} shrank while sending", s.sources[i].display()),
            )));
        }
        hasher.update(&buf[..n]);
        stream.write_all(&buf[..n]).await.map_err(conn_err)?;
        remaining -= n as u64;
        s.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    stream.write_all(hasher.finalize().as_bytes()).await.map_err(conn_err)?;
    stream.finish().map_err(conn_err)?;
    let _ = s.events.send(TransferEvent::FileCompleted { path: entry.path.clone() });
    Ok(())
}
