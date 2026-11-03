mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{data, pair, snapshot, write};
use peerly_proto::frame::{read_frame, write_frame};
use peerly_proto::transfer::{Control, FileEntry, Manifest, StreamHeader};
use peerly_transfer::{Summary, TransferError, TransferEvent};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::sync::CancellationToken;

struct Outcome {
    send: Result<Summary, TransferError>,
    recv: Result<Summary, TransferError>,
    send_events: Vec<TransferEvent>,
    recv_events: Vec<TransferEvent>,
}

fn drain(mut rx: UnboundedReceiver<TransferEvent>) -> Vec<TransferEvent> {
    let mut v = Vec::new();
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}

async fn transfer(paths: Vec<PathBuf>, dest: &Path, accept: bool) -> Outcome {
    let p = pair().await;
    let (stx, srx) = unbounded_channel();
    let (rtx, rrx) = unbounded_channel();
    let (send, recv) = tokio::join!(
        peerly_transfer::send(&p.sender, &paths, stx, CancellationToken::new()),
        peerly_transfer::receive(&p.receiver, dest, |_| async move { accept }, rtx, CancellationToken::new()),
    );
    Outcome { send, recv, send_events: drain(srx), recv_events: drain(rrx) }
}

fn assert_ok(o: &Outcome) {
    assert!(o.send.is_ok(), "send: {:?}", o.send);
    assert!(o.recv.is_ok(), "recv: {:?}", o.recv);
}

fn no_part_files(dir: &Path) -> bool {
    snapshot(dir).keys().all(|k| !k.ends_with(".peerly-part"))
}

#[tokio::test]
async fn single_file() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let f = src.path().join("hello.bin");
    let bytes = data(300_000, 1);
    write(&f, &bytes);
    let o = transfer(vec![f], dst.path(), true).await;
    assert_ok(&o);
    assert_eq!(std::fs::read(dst.path().join("hello.bin")).unwrap(), bytes);
    let s = o.recv.unwrap();
    assert_eq!((s.files, s.bytes), (1, 300_000));

    let TransferEvent::Offered(m) = &o.recv_events[0] else { panic!("{:?}", o.recv_events) };
    assert_eq!(m.name, "hello.bin");
    for evs in [&o.send_events, &o.recv_events] {
        assert!(evs.contains(&TransferEvent::Started { total_bytes: 300_000, total_files: 1 }));
        assert!(evs.contains(&TransferEvent::FileCompleted { path: "hello.bin".into() }));
        let last = evs.iter().rev().find(|e| matches!(e, TransferEvent::Progress { .. }));
        assert_eq!(last, Some(&TransferEvent::Progress { bytes_done: 300_000, bytes_total: 300_000 }));
    }
}

#[tokio::test]
async fn nested_folder_with_empty_dir_and_zero_byte_file() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let root = src.path().join("photos");
    write(&root.join("a.jpg"), &data(1000, 2));
    write(&root.join("2024/b.jpg"), &data(70_000, 3));
    write(&root.join("2024/deep/c.txt"), b"");
    std::fs::create_dir_all(root.join("empty/inner")).unwrap();
    let extra = src.path().join("zero.dat");
    write(&extra, b"");

    let o = transfer(vec![root.clone(), extra], dst.path(), true).await;
    assert_ok(&o);
    let TransferEvent::Offered(m) = &o.recv_events[0] else { panic!() };
    assert_eq!(m.name, "2 items");
    assert_eq!(m.empty_dirs, ["photos/empty/inner"]);
    assert_eq!(snapshot(&root), snapshot(&dst.path().join("photos")));
    assert_eq!(std::fs::read(dst.path().join("zero.dat")).unwrap(), b"");
}

#[tokio::test]
async fn many_small_files() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let root = src.path().join("album");
    for i in 0..500u64 {
        write(&root.join(format!("d{}/img_{i:04}.jpg", i % 10)), &data(1000 + (i as usize * 37) % 4000, i));
    }
    let t = std::time::Instant::now();
    let o = transfer(vec![root.clone()], dst.path(), true).await;
    assert_ok(&o);
    println!("500 small files in {:?}", t.elapsed());
    assert_eq!(o.recv.as_ref().unwrap().files, 500);
    assert_eq!(snapshot(&root), snapshot(&dst.path().join("album")));
    assert!(no_part_files(dst.path()));
}

#[tokio::test]
async fn reject() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let f = src.path().join("x.txt");
    write(&f, b"nope");
    let o = transfer(vec![f], dst.path(), false).await;
    assert!(matches!(o.send, Err(TransferError::Rejected)), "{:?}", o.send);
    assert!(matches!(o.recv, Err(TransferError::Rejected)), "{:?}", o.recv);
    assert!(snapshot(dst.path()).is_empty());
}

#[tokio::test]
async fn name_collision_gets_suffix() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    write(&src.path().join("report.pdf"), b"new");
    write(&src.path().join("photos/a.jpg"), b"new-a");
    write(&dst.path().join("report.pdf"), b"old");
    write(&dst.path().join("photos/a.jpg"), b"old-a");
    let o = transfer(vec![src.path().join("report.pdf"), src.path().join("photos")], dst.path(), true).await;
    assert_ok(&o);
    let snap = snapshot(dst.path());
    assert_eq!(snap["report.pdf"].as_deref(), Some(&b"old"[..]));
    assert_eq!(snap["report (1).pdf"].as_deref(), Some(&b"new"[..]));
    assert_eq!(snap["photos/a.jpg"].as_deref(), Some(&b"old-a"[..]));
    assert_eq!(snap["photos (1)/a.jpg"].as_deref(), Some(&b"new-a"[..]));
}

/// Cancel one side as soon as bytes flow; the receiver must not leave `.peerly-part` files.
async fn cancel_mid_transfer(cancel_receiver: bool) {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let root = src.path().join("big");
    for i in 0..4 {
        write(&root.join(format!("f{i}.bin")), &data(32 << 20, i));
    }
    let p = pair().await;
    let (stx, mut srx) = unbounded_channel();
    let (rtx, mut rrx) = unbounded_channel();
    let token = CancellationToken::new();
    let (st, rt) = if cancel_receiver {
        (CancellationToken::new(), token.clone())
    } else {
        (token.clone(), CancellationToken::new())
    };
    let paths = vec![root];
    let canceller = async {
        let rx = if cancel_receiver { &mut rrx } else { &mut srx };
        while let Some(e) = rx.recv().await {
            if matches!(e, TransferEvent::Progress { bytes_done, .. } if bytes_done > 0) {
                token.cancel();
                break;
            }
        }
    };
    let (send, recv, _) = tokio::join!(
        peerly_transfer::send(&p.sender, &paths, stx, st),
        peerly_transfer::receive(&p.receiver, dst.path(), |_| async { true }, rtx, rt),
        canceller,
    );
    let (mine, theirs) = if cancel_receiver { (&recv, &send) } else { (&send, &recv) };
    assert!(matches!(mine, Err(TransferError::Cancelled)), "{mine:?}");
    assert!(matches!(theirs, Err(TransferError::PeerCancelled(_))), "{theirs:?}");
    assert!(no_part_files(dst.path()), "{:?}", snapshot(dst.path()).keys().collect::<Vec<_>>());
}

#[tokio::test]
async fn receiver_cancel_cleans_part_files() {
    cancel_mid_transfer(true).await;
}

#[tokio::test]
async fn sender_cancel_cleans_part_files() {
    cancel_mid_transfer(false).await;
}

/// Hand-rolled malicious sender: speaks the protocol directly.
async fn evil_sender(conn: &quinn::Connection, m: Manifest, payload: &[u8], trailer: [u8; 32]) -> Control {
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    write_frame(&mut tx, &Control::Offer(m)).await.unwrap();
    let reply: Control = read_frame(&mut rx).await.unwrap();
    if reply == Control::Accept {
        let mut s = conn.open_uni().await.unwrap();
        write_frame(&mut s, &StreamHeader { index: 0, offset: 0 }).await.unwrap();
        s.write_all(payload).await.unwrap();
        s.write_all(&trailer).await.unwrap();
        s.finish().unwrap();
        return tokio::time::timeout(Duration::from_secs(5), read_frame(&mut rx))
            .await
            .unwrap()
            .unwrap_or(Control::Cancel { reason: "conn closed".into() });
    }
    reply
}

fn one_file(path: &str, size: u64) -> Manifest {
    Manifest {
        name: path.into(),
        files: vec![FileEntry { index: 0, path: path.into(), size }],
        empty_dirs: vec![],
        total_size: size,
    }
}

#[tokio::test]
async fn malicious_path_is_refused() {
    let dst = tempfile::tempdir().unwrap();
    let p = pair().await;
    let (rtx, _rrx) = unbounded_channel();
    let (reply, recv) = tokio::join!(
        evil_sender(&p.sender, one_file("../escape.txt", 3), b"abc", [0; 32]),
        peerly_transfer::receive(&p.receiver, dst.path(), |_| async { true }, rtx, CancellationToken::new()),
    );
    assert!(matches!(recv, Err(TransferError::UnsafePath(_))), "{recv:?}");
    assert!(matches!(reply, Control::Cancel { .. }), "{reply:?}");
    assert!(snapshot(dst.path()).is_empty());
}

#[tokio::test]
async fn hash_mismatch_is_detected() {
    let dst = tempfile::tempdir().unwrap();
    let p = pair().await;
    let (rtx, _rrx) = unbounded_channel();
    let (reply, recv) = tokio::join!(
        evil_sender(&p.sender, one_file("x.bin", 3), b"abc", [7; 32]),
        peerly_transfer::receive(&p.receiver, dst.path(), |_| async { true }, rtx, CancellationToken::new()),
    );
    assert!(matches!(recv, Err(TransferError::HashMismatch(_))), "{recv:?}");
    assert!(matches!(reply, Control::Cancel { .. }), "{reply:?}");
    assert!(snapshot(dst.path()).is_empty(), "{:?}", snapshot(dst.path()).keys().collect::<Vec<_>>());
}

#[tokio::test]
async fn short_stream_is_protocol_error() {
    let dst = tempfile::tempdir().unwrap();
    let p = pair().await;
    let (rtx, _rrx) = unbounded_channel();
    let hash = *blake3::hash(b"ab").as_bytes();
    let (_, recv) = tokio::join!(
        evil_sender(&p.sender, one_file("x.bin", 40), b"ab", hash),
        peerly_transfer::receive(&p.receiver, dst.path(), |_| async { true }, rtx, CancellationToken::new()),
    );
    assert!(matches!(recv, Err(TransferError::Protocol(_))), "{recv:?}");
    assert!(snapshot(dst.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_file_throughput() {
    const SIZE: usize = 64 << 20;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let f = src.path().join("big.bin");
    let bytes = data(SIZE, 42);
    write(&f, &bytes);
    let o = transfer(vec![f], dst.path(), true).await;
    assert_ok(&o);
    let s = o.send.unwrap();
    let mbps = SIZE as f64 / (1024.0 * 1024.0) / s.elapsed.as_secs_f64();
    println!("64 MiB in {:?} -> {mbps:.1} MiB/s ({:.0} Mbit/s)", s.elapsed, mbps * 8.388_608);
    let got = std::fs::read(dst.path().join("big.bin")).unwrap();
    assert!(got == bytes, "content differs");
}
