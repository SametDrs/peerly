use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use peerly_proto::probe::{ProbeRequest, ProbeResponse, MAGIC};
use peerly_proto::rendezvous::{ClientMsg, ErrorCode, ServerMsg};
use peerly_proto::{decode, encode, PROTOCOL_VERSION};
use peerly_rendezvous::{Server, ServerConfig};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(f: impl FnOnce(&mut ServerConfig)) -> std::net::SocketAddr {
    let mut cfg = ServerConfig { listen: "127.0.0.1:0".parse().unwrap(), ..Default::default() };
    f(&mut cfg);
    let s = Server::bind(cfg).await.unwrap();
    let a = s.local_addr();
    tokio::spawn(s.run());
    a
}

async fn tx(ws: &mut Ws, m: ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(&m).unwrap().into())).await.unwrap();
}

async fn rx(ws: &mut Ws) -> ServerMsg {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .expect("closed")
            .expect("err");
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

async fn client(a: std::net::SocketAddr) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{a}")).await.unwrap();
    tx(&mut ws, ClientMsg::Hello { version: PROTOCOL_VERSION }).await;
    assert!(matches!(rx(&mut ws).await, ServerMsg::Welcome { .. }));
    ws
}

async fn pair(a: std::net::SocketAddr) -> (Ws, Ws, u32) {
    let mut x = client(a).await;
    tx(&mut x, ClientMsg::Allocate).await;
    let ServerMsg::Allocated { nameplate } = rx(&mut x).await else { panic!() };
    let mut y = client(a).await;
    tx(&mut y, ClientMsg::Claim { nameplate }).await;
    assert_eq!(rx(&mut y).await, ServerMsg::Claimed);
    assert_eq!(rx(&mut x).await, ServerMsg::PeerJoined);
    (x, y, nameplate)
}

fn is_err(m: ServerMsg, c: ErrorCode) -> bool {
    matches!(m, ServerMsg::Error { code, .. } if code == c)
}

#[tokio::test]
async fn version_mismatch() {
    let a = start(|_| {}).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{a}")).await.unwrap();
    tx(&mut ws, ClientMsg::Hello { version: 999 }).await;
    assert!(is_err(rx(&mut ws).await, ErrorCode::VersionMismatch));
    loop {
        match ws.next().await {
            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn allocate_claim_relay_and_leave() {
    let a = start(|_| {}).await;
    let (mut x, mut y, n) = pair(a).await;
    assert!((1..=99).contains(&n));
    tx(&mut x, ClientMsg::Relay { payload: vec![1, 2] }).await;
    assert_eq!(rx(&mut y).await, ServerMsg::Peer { payload: vec![1, 2] });
    tx(&mut y, ClientMsg::Relay { payload: vec![3] }).await;
    assert_eq!(rx(&mut x).await, ServerMsg::Peer { payload: vec![3] });
    tx(&mut y, ClientMsg::Close).await;
    assert_eq!(rx(&mut x).await, ServerMsg::PeerLeft);
    // single-use: gone
    let mut z = client(a).await;
    tx(&mut z, ClientMsg::Claim { nameplate: n }).await;
    assert!(is_err(rx(&mut z).await, ErrorCode::NameplateNotFound));
}

#[tokio::test]
async fn peer_left_on_disconnect_and_no_peer() {
    let a = start(|_| {}).await;
    let mut x = client(a).await;
    tx(&mut x, ClientMsg::Allocate).await;
    let ServerMsg::Allocated { nameplate } = rx(&mut x).await else { panic!() };
    tx(&mut x, ClientMsg::Relay { payload: vec![1] }).await;
    assert!(is_err(rx(&mut x).await, ErrorCode::NoPeer));
    let mut y = client(a).await;
    tx(&mut y, ClientMsg::Claim { nameplate }).await;
    assert_eq!(rx(&mut y).await, ServerMsg::Claimed);
    assert_eq!(rx(&mut x).await, ServerMsg::PeerJoined);
    drop(y);
    assert_eq!(rx(&mut x).await, ServerMsg::PeerLeft);
}

#[tokio::test]
async fn unknown_and_full() {
    let a = start(|_| {}).await;
    let mut c = client(a).await;
    tx(&mut c, ClientMsg::Claim { nameplate: 424242 }).await;
    assert!(is_err(rx(&mut c).await, ErrorCode::NameplateNotFound));
    let (_x, _y, n) = pair(a).await;
    let mut z = client(a).await;
    tx(&mut z, ClientMsg::Claim { nameplate: n }).await;
    assert!(is_err(rx(&mut z).await, ErrorCode::NameplateFull));
}

#[tokio::test]
async fn expiry() {
    let a = start(|c| c.nameplate_ttl = Duration::from_millis(100)).await;
    let mut x = client(a).await;
    tx(&mut x, ClientMsg::Allocate).await;
    let ServerMsg::Allocated { nameplate } = rx(&mut x).await else { panic!() };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut y = client(a).await;
    tx(&mut y, ClientMsg::Claim { nameplate }).await;
    assert!(is_err(rx(&mut y).await, ErrorCode::NameplateNotFound));
}

#[tokio::test]
async fn relay_size_limit() {
    let a = start(|_| {}).await;
    let (mut x, mut y, _) = pair(a).await;
    tx(&mut x, ClientMsg::Relay { payload: vec![0; 9000] }).await;
    assert!(is_err(rx(&mut x).await, ErrorCode::BadRequest));
    tx(&mut x, ClientMsg::Relay { payload: vec![0; 8192] }).await;
    assert!(matches!(rx(&mut y).await, ServerMsg::Peer { .. }));
}

#[tokio::test]
async fn rate_limit() {
    let a = start(|c| c.rate_limit_per_min = 3).await;
    let mut c = client(a).await;
    for _ in 0..3 {
        tx(&mut c, ClientMsg::Claim { nameplate: 5 }).await;
        assert!(is_err(rx(&mut c).await, ErrorCode::NameplateNotFound));
    }
    tx(&mut c, ClientMsg::Claim { nameplate: 5 }).await;
    assert!(is_err(rx(&mut c).await, ErrorCode::RateLimited));
}

#[tokio::test]
async fn udp_probe() {
    let a = start(|_| {}).await;
    let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    s.send_to(b"garbage", a).await.unwrap();
    let nonce = [7u8; 16];
    s.send_to(&encode(&ProbeRequest { magic: MAGIC, nonce }).unwrap(), a).await.unwrap();
    let mut buf = [0u8; 256];
    let (n, _) =
        tokio::time::timeout(Duration::from_secs(5), s.recv_from(&mut buf)).await.unwrap().unwrap();
    let r: ProbeResponse = decode(&buf[..n]).unwrap();
    assert_eq!(r.nonce, nonce);
    assert_eq!(r.observed, s.local_addr().unwrap());
}
