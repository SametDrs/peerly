//! End-to-end pairing tests against an in-process mock rendezvous server on 127.0.0.1.

mod mock_rendezvous;

use std::time::Duration;

use peerly_pairing::code::{Code, wordlist};
use peerly_pairing::quic::{Identity, client_config, server_config};
use peerly_pairing::{Config, PairingError, Route, allocate, join};
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(40);

async fn cfg() -> Config {
    Config { rendezvous_url: mock_rendezvous::start().await }
}

#[tokio::test]
async fn pair_and_echo() {
    let cfg = cfg().await;
    let pending = allocate(&cfg).await.unwrap();
    let code = pending.code().to_string();
    assert!(Code::parse(&code).is_some(), "{code}");

    let sender = tokio::spawn(async move {
        let p = pending.wait_for_peer(CancellationToken::new()).await.unwrap();
        let (mut send, mut recv) = p.connection.accept_bi().await.unwrap();
        let data = recv.read_to_end(1 << 20).await.unwrap();
        send.write_all(&data).await.unwrap();
        send.finish().unwrap();
        let _ = p.connection.closed().await;
        p.route
    });
    let p = tokio::time::timeout(T, join(&cfg, &code.to_uppercase(), CancellationToken::new()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.route, Route::Lan);
    let (mut send, mut recv) = p.connection.open_bi().await.unwrap();
    send.write_all(b"hello peerly").await.unwrap();
    send.finish().unwrap();
    let echoed = tokio::time::timeout(T, recv.read_to_end(1 << 20)).await.unwrap().unwrap();
    assert_eq!(echoed, b"hello peerly");
    p.connection.close(0u32.into(), b"done");
    let sender_route = tokio::time::timeout(T, sender).await.unwrap().unwrap();
    assert_eq!(sender_route, Route::Lan);
}

#[tokio::test]
async fn wrong_words_fail_on_both_sides() {
    let cfg = cfg().await;
    let pending = allocate(&cfg).await.unwrap();
    let mut code = Code::parse(pending.code()).unwrap();
    code.words[1] = wordlist().iter().find(|w| **w != code.words[1]).unwrap().to_string();

    let sender = tokio::spawn(pending.wait_for_peer(CancellationToken::new()));
    let r = tokio::time::timeout(T, join(&cfg, &code.to_string(), CancellationToken::new())).await.unwrap();
    assert!(matches!(r, Err(PairingError::WrongCode)), "receiver: {:?}", r.err());
    let s = tokio::time::timeout(T, sender).await.unwrap().unwrap();
    assert!(matches!(s, Err(PairingError::WrongCode)), "sender: {:?}", s.err());
}

#[tokio::test]
async fn unknown_nameplate() {
    let cfg = cfg().await;
    let r = join(&cfg, "9999-river-candle", CancellationToken::new()).await;
    assert!(matches!(r, Err(PairingError::CodeNotFound)), "{:?}", r.err());
}

#[tokio::test]
async fn nameplate_already_claimed() {
    let cfg = cfg().await;
    let pending = allocate(&cfg).await.unwrap();
    let code = pending.code().to_string();
    let first = {
        let (cfg, code) = (cfg.clone(), code.clone());
        tokio::spawn(async move { join(&cfg, &code, CancellationToken::new()).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = join(&cfg, &code, CancellationToken::new()).await;
    assert!(matches!(r, Err(PairingError::CodeInUse)), "{:?}", r.err());
    first.abort();
    drop(pending);
}

#[tokio::test]
async fn invalid_code_and_unreachable_server() {
    let cfg = cfg().await;
    let r = join(&cfg, "not a code", CancellationToken::new()).await;
    assert!(matches!(r, Err(PairingError::InvalidCode)));

    let dead = Config { rendezvous_url: "ws://127.0.0.1:1".into() };
    let r = allocate(&dead).await;
    assert!(matches!(r, Err(PairingError::RendezvousUnreachable(_))), "{:?}", r.err());
}

#[tokio::test]
async fn cancel_while_waiting() {
    let cfg = cfg().await;
    let pending = allocate(&cfg).await.unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(pending.wait_for_peer(cancel.clone()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let r = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert!(matches!(r, Err(PairingError::Cancelled)), "{:?}", r.err());

    // Cancelling the receiver also works (token already cancelled).
    let pending = allocate(&cfg).await.unwrap();
    let r = join(&cfg, pending.code(), cancel).await;
    assert!(matches!(r, Err(PairingError::Cancelled)));
}

/// Handshake on loopback with explicit identities; returns (client ok, server ok).
async fn handshake(server_id: &Identity, server_pins: [u8; 32], client_id: &Identity, client_pins: [u8; 32]) -> (bool, bool) {
    let server = quinn::Endpoint::server(server_config(server_id, server_pins).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(client_config(client_id, client_pins).unwrap());

    let srv = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let ok = incoming.await.is_ok();
        (ok, server)
    });
    let conn = tokio::time::timeout(T, client.connect(addr, "peerly").unwrap()).await.unwrap();
    let client_ok = conn.is_ok();
    let (server_ok, _server) = tokio::time::timeout(T, srv).await.unwrap().unwrap();
    (client_ok, server_ok)
}

#[tokio::test]
async fn certificate_pinning() {
    let sender = Identity::generate().unwrap();
    let receiver = Identity::generate().unwrap();
    let impostor = Identity::generate().unwrap();

    // Control: correct pins on both sides.
    assert_eq!(handshake(&sender, receiver.fingerprint, &receiver, sender.fingerprint).await, (true, true));
    // A client presenting a different certificate is rejected by the server.
    let (_, server_ok) = handshake(&sender, receiver.fingerprint, &impostor, sender.fingerprint).await;
    assert!(!server_ok, "server accepted an unpinned client certificate");
    // A server presenting a different certificate is rejected by the client.
    let (client_ok, _) = handshake(&impostor, receiver.fingerprint, &receiver, sender.fingerprint).await;
    assert!(!client_ok, "client accepted an unpinned server certificate");
}
