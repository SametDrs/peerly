//! LAN mailbox tests over loopback, bypassing mDNS discovery (real QUIC).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::code::{Code, wordlist};
use crate::{
    Config, Env, PairingError, PendingSend, Route, Via, allocate_with, connect_as_receiver, lan,
};

const T: Duration = Duration::from_secs(40);

fn no_mdns() -> Env {
    Env {
        mdns: false,
        ..Env::default()
    }
}

fn lan_only() -> Config {
    Config {
        rendezvous_url: None,
        lan: true,
        dht: false,
    }
}

async fn sender() -> (PendingSend, Code, SocketAddr) {
    let pending = allocate_with(&lan_only(), &no_mdns()).await.unwrap();
    let code = Code::parse(pending.code()).unwrap();
    assert!((1000..=9999).contains(&code.nameplate), "{code}");
    let port = pending.lan_port().expect("LAN listener");
    (pending, code, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

fn wrong(code: &Code) -> Code {
    let mut w = code.clone();
    w.words[1] = wordlist()
        .iter()
        .find(|x| **x != code.words[1])
        .unwrap()
        .to_string();
    w
}

/// True once nothing accepts TCP connections on `addr` any more.
async fn closed(addr: SocketAddr) -> bool {
    // Windows retries refused loopback connects for ~2 s before failing.
    !matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(addr)
        )
        .await,
        Ok(Ok(_))
    )
}

async fn receive_echo(code: &Code, addr: SocketAddr) -> Route {
    let (local, peer) = lan::exchange_at(code, &[addr]).await.unwrap();
    let p = connect_as_receiver(local, peer, Via::Lan).await.unwrap();
    let (mut send, mut recv) = p.connection.open_bi().await.unwrap();
    send.write_all(b"hello lan").await.unwrap();
    send.finish().unwrap();
    let echoed = tokio::time::timeout(T, recv.read_to_end(1 << 20))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, b"hello lan");
    p.connection.close(0u32.into(), b"done");
    p.route
}

fn spawn_echo_sender(pending: PendingSend) -> tokio::task::JoinHandle<Result<Route, PairingError>> {
    tokio::spawn(async move {
        let p = pending.wait_for_peer(CancellationToken::new()).await?;
        let (mut send, mut recv) = p.connection.accept_bi().await.unwrap();
        let data = recv.read_to_end(1 << 20).await.unwrap();
        send.write_all(&data).await.unwrap();
        send.finish().unwrap();
        let _ = p.connection.closed().await;
        Ok(p.route)
    })
}

#[tokio::test]
async fn lan_pair_and_echo() {
    let (pending, code, addr) = sender().await;
    let task = spawn_echo_sender(pending);
    let route = tokio::time::timeout(T, receive_echo(&code, addr))
        .await
        .unwrap();
    assert_eq!(route, Route::Lan);
    let sender_route = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert_eq!(sender_route.unwrap(), Route::Lan);
    assert!(closed(addr).await, "LAN listener still open after pairing");
}

#[tokio::test]
async fn lan_tolerates_wrong_codes_then_pairs() {
    let (pending, code, addr) = sender().await;
    let task = spawn_echo_sender(pending);
    for _ in 0..2 {
        let r = tokio::time::timeout(T, lan::exchange_at(&wrong(&code), &[addr]))
            .await
            .unwrap();
        assert!(matches!(r, Err(PairingError::WrongCode)), "{:?}", r.err());
    }
    let route = tokio::time::timeout(T, receive_echo(&code, addr))
        .await
        .unwrap();
    assert_eq!(route, Route::Lan);
    assert_eq!(
        tokio::time::timeout(T, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Route::Lan
    );
}

#[tokio::test]
async fn lan_aborts_after_too_many_wrong_codes() {
    let (pending, code, addr) = sender().await;
    let task = tokio::spawn(pending.wait_for_peer(CancellationToken::new()));
    for i in 0..=lan::MAX_WRONG_CODES {
        let r = tokio::time::timeout(T, lan::exchange_at(&wrong(&code), &[addr]))
            .await
            .unwrap();
        assert!(
            matches!(r, Err(PairingError::WrongCode)),
            "attempt {i}: {:?}",
            r.err()
        );
    }
    let s = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert!(matches!(s, Err(PairingError::WrongCode)), "{:?}", s.err());
    assert!(closed(addr).await, "LAN listener still open after abort");
}

#[tokio::test]
async fn lan_ignores_foreign_hello() {
    use peerly_proto::frame::write_frame;
    use peerly_proto::lan::LanHello;

    let (pending, code, addr) = sender().await;
    let task = spawn_echo_sender(pending);
    // A receiver looking for another nameplate (or a port scanner) is dropped
    // without costing the sender a wrong-code attempt.
    for _ in 0..6 {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let hello = LanHello {
            version: peerly_proto::PROTOCOL_VERSION,
            nameplate: code.nameplate + 1000,
        };
        write_frame(&mut s, &hello).await.unwrap();
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(T, tokio::io::AsyncReadExt::read(&mut s, &mut buf))
            .await
            .unwrap()
            .unwrap_or(0);
        assert_eq!(n, 0, "sender answered a foreign hello");
    }
    let route = tokio::time::timeout(T, receive_echo(&code, addr))
        .await
        .unwrap();
    assert_eq!(route, Route::Lan);
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancel_and_drop_tear_down_listener() {
    let (pending, _code, addr) = sender().await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(pending.wait_for_peer(cancel.clone()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_ok(),
        "listener should be up while waiting"
    );
    cancel.cancel();
    let r = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert!(matches!(r, Err(PairingError::Cancelled)), "{:?}", r.err());
    assert!(closed(addr).await, "LAN listener still open after cancel");

    let (pending, _code, addr) = sender().await;
    drop(pending);
    assert!(closed(addr).await, "LAN listener still open after drop");
}

#[tokio::test]
async fn unreachable_rendezvous_falls_back_to_lan() {
    let cfg = Config {
        rendezvous_url: Some("ws://127.0.0.1:1".into()),
        lan: true,
        dht: false,
    };
    let pending = allocate_with(&cfg, &no_mdns()).await.unwrap();
    let np = Code::parse(pending.code()).unwrap().nameplate;
    assert!((1000..=9999).contains(&np));
    assert!(pending.lan_port().is_some());
}

#[tokio::test]
async fn nothing_configured_is_an_error() {
    let cfg = Config {
        rendezvous_url: None,
        lan: false,
        dht: false,
    };
    let r = allocate_with(&cfg, &no_mdns()).await;
    assert!(
        matches!(r, Err(PairingError::RendezvousUnreachable(_))),
        "{:?}",
        r.err()
    );
}
