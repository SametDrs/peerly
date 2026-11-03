//! Needs a running rendezvous server (`cargo run -p peerly-rendezvous`); run with `--ignored`.
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore]
async fn send_produces_code() {
    let dir = std::env::temp_dir().join("peerly-desktop-smoke.txt");
    std::fs::write(&dir, b"hi").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let task = tokio::spawn(async move {
        peerly_core::send(&peerly_core::Config::from_env(), vec![dir], tx, c2).await
    });
    let ev = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, peerly_core::Event::Code { .. }), "{ev:?}");
    cancel.cancel();
    let _ = task.await;
}
