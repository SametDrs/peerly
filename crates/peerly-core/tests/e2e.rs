use std::path::{Path, PathBuf};
use std::time::Duration;

use peerly_core::{Config, Error, Event};
use peerly_rendezvous::{Server, ServerConfig};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::sync::CancellationToken;

async fn start_server() -> Config {
    let cfg = ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        ..Default::default()
    };
    let server = Server::bind(cfg).await.unwrap();
    let addr = server.local_addr();
    tokio::spawn(server.run());
    Config {
        rendezvous_url: format!("ws://{addr}"),
    }
}

async fn next_code(rx: &mut UnboundedReceiver<Event>) -> String {
    while let Some(ev) = rx.recv().await {
        if let Event::Code { code } = ev {
            return code;
        }
    }
    panic!("no code event");
}

fn make_tree(root: &Path) -> PathBuf {
    let dir = root.join("payload");
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    std::fs::write(dir.join("small.txt"), b"hello").unwrap();
    std::fs::write(dir.join("a/b/nested.txt"), b"nested file").unwrap();
    let big: Vec<u8> = (0..20_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(dir.join("a/big.bin"), big).unwrap();
    dir
}

fn same_tree(a: &Path, b: &Path) {
    for e in std::fs::read_dir(a).unwrap() {
        let e = e.unwrap();
        let o = b.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            same_tree(&e.path(), &o);
        } else {
            assert!(
                std::fs::read(e.path()).unwrap() == std::fs::read(&o).unwrap(),
                "{o:?} differs"
            );
        }
    }
}

#[tokio::test]
async fn roundtrip_folder() {
    let cfg = start_server().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = make_tree(src.path());

    let (stx, mut srx) = unbounded_channel();
    let c = cfg.clone();
    let d2 = dir.clone();
    let sender = tokio::spawn(async move {
        peerly_core::send(&c, vec![d2], stx, CancellationToken::new()).await
    });
    let code = next_code(&mut srx).await;

    let (rtx, mut rrx) = unbounded_channel();
    let dest = dst.path().to_path_buf();
    peerly_core::receive(
        &cfg,
        &code,
        &dest,
        |_m| async { true },
        rtx,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    sender.await.unwrap().unwrap();

    same_tree(&dir, &dst.path().join("payload"));
    let mut saw_offer = false;
    let mut completed = false;
    while let Ok(ev) = rrx.try_recv() {
        match ev {
            Event::Offer { files, .. } => {
                assert_eq!(files, 3);
                saw_offer = true;
            }
            Event::Completed { files, .. } => {
                assert_eq!(files, 3);
                completed = true;
            }
            _ => {}
        }
    }
    assert!(saw_offer && completed);
}

#[tokio::test]
async fn wrong_code_errors() {
    let cfg = start_server().await;
    let src = tempfile::tempdir().unwrap();
    let f = src.path().join("f.txt");
    std::fs::write(&f, b"x").unwrap();
    let (stx, mut srx) = unbounded_channel();
    let c = cfg.clone();
    let sender =
        tokio::spawn(
            async move { peerly_core::send(&c, vec![f], stx, CancellationToken::new()).await },
        );
    let code = next_code(&mut srx).await;
    let nameplate = peerly_pairing::code::Code::parse(&code).unwrap().nameplate;
    let wrong = peerly_pairing::code::Code::generate(nameplate).to_string();
    assert_ne!(wrong, code);

    let (rtx, _rrx) = unbounded_channel();
    let dst = tempfile::tempdir().unwrap();
    let r = peerly_core::receive(
        &cfg,
        &wrong,
        dst.path(),
        |_| async { true },
        rtx,
        CancellationToken::new(),
    )
    .await;
    assert!(r.is_err(), "receiver should fail");
    let s = tokio::time::timeout(Duration::from_secs(30), sender)
        .await
        .expect("sender hung")
        .unwrap();
    assert!(s.is_err(), "sender should fail");
}

#[tokio::test]
async fn decline_rejects_sender() {
    let cfg = start_server().await;
    let src = tempfile::tempdir().unwrap();
    let f = src.path().join("f.txt");
    std::fs::write(&f, b"x").unwrap();
    let (stx, mut srx) = unbounded_channel();
    let c = cfg.clone();
    let sender =
        tokio::spawn(
            async move { peerly_core::send(&c, vec![f], stx, CancellationToken::new()).await },
        );
    let code = next_code(&mut srx).await;
    let (rtx, _rrx) = unbounded_channel();
    let dst = tempfile::tempdir().unwrap();
    let r = peerly_core::receive(
        &cfg,
        &code,
        dst.path(),
        |_| async { false },
        rtx,
        CancellationToken::new(),
    )
    .await;
    assert!(r.is_ok());
    let s = sender.await.unwrap();
    assert!(
        matches!(
            s,
            Err(Error::Transfer(peerly_transfer::TransferError::Rejected))
        ),
        "{s:?}"
    );
}

#[tokio::test]
async fn cancel_mid_transfer() {
    let cfg = start_server().await;
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = make_tree(src.path());
    let (stx, mut srx) = unbounded_channel();
    let c = cfg.clone();
    let cancel = CancellationToken::new();
    let sc = cancel.clone();
    let sender = tokio::spawn(async move { peerly_core::send(&c, vec![dir], stx, sc).await });
    let code = next_code(&mut srx).await;
    let (rtx, mut rrx) = unbounded_channel();
    let dest = dst.path().to_path_buf();
    let receiver = tokio::spawn(async move {
        peerly_core::receive(
            &cfg,
            &code,
            &dest,
            |_| async { true },
            rtx,
            CancellationToken::new(),
        )
        .await
    });
    while let Some(ev) = rrx.recv().await {
        if matches!(ev, Event::Progress { .. }) {
            cancel.cancel();
            break;
        }
    }
    let s = tokio::time::timeout(Duration::from_secs(30), sender)
        .await
        .expect("sender hung")
        .unwrap();
    assert!(matches!(s, Err(Error::Cancelled)), "{s:?}");
    let r = tokio::time::timeout(Duration::from_secs(30), receiver)
        .await
        .expect("receiver hung")
        .unwrap();
    assert!(r.is_err(), "receiver should error after peer cancel");
}
