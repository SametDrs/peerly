//! DHT mailbox tests on a local mainline testnet (offline, real QUIC, no STUN).

use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mainline::Testnet;
use peerly_proto::PROTOCOL_VERSION;
use peerly_proto::dht::{DhtItem, ITEM_MAX_AGE_SECS, SALT_RECEIVER_PAKE, SALT_SENDER_PAKE};
use peerly_proto::pairing::PairingMsg;
use tokio_util::sync::CancellationToken;

use crate::code::{Code, wordlist};
use crate::dht::signing_key;
use crate::{Config, Env, PairingError, PendingSend, Route, allocate_with, join_with};

const T: Duration = Duration::from_secs(90);

/// A private DHT; dropping it shuts the nodes down.
struct Net {
    _testnet: Testnet,
    env: Env,
}

fn net() -> Net {
    let testnet = Testnet::builder(10).build().unwrap();
    let env = Env {
        mdns: false,
        dht_bootstrap: Some(testnet.bootstrap.clone()),
        stun: Vec::new(),
        ..Env::default()
    };
    Net {
        _testnet: testnet,
        env,
    }
}

fn dht_only() -> Config {
    Config {
        rendezvous_url: None,
        lan: false,
        dht: true,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Writes a raw item into a nameplate's slot, as another (stale or foreign) peer would.
async fn plant(env: &Env, nameplate: u32, salt: &[u8], session: [u8; 16], created: u64) {
    let node = env.dht_node().unwrap();
    let item = DhtItem {
        version: PROTOCOL_VERSION,
        session,
        created,
        msg: PairingMsg::Pake(vec![9; 32]),
    };
    node.put(&signing_key(nameplate), salt, &item)
        .await
        .unwrap();
}

fn force_nameplates(env: &Env, nps: &[u32]) {
    env.nameplates.lock().unwrap().extend_from_slice(nps);
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

/// Full pairing over the DHT and an echo over the resulting QUIC connection.
async fn pair_and_echo(env: &Env, pending: PendingSend) {
    let code = pending.code().to_string();
    let sender = spawn_echo_sender(pending);
    let p = tokio::time::timeout(
        T,
        join_with(&dht_only(), &code, CancellationToken::new(), env),
    )
    .await
    .unwrap()
    .unwrap();
    let (mut send, mut recv) = p.connection.open_bi().await.unwrap();
    send.write_all(b"hello dht").await.unwrap();
    send.finish().unwrap();
    let echoed = tokio::time::timeout(T, recv.read_to_end(1 << 20))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, b"hello dht");
    p.connection.close(0u32.into(), b"done");
    // Same machine: the connecting address is a local one.
    assert_eq!(p.route, Route::Lan);
    let s = tokio::time::timeout(T, sender).await.unwrap().unwrap();
    assert_eq!(s.unwrap(), Route::Lan);
}

#[tokio::test]
async fn dht_pair_and_echo() {
    let net = net();
    let pending = allocate_with(&dht_only(), &net.env).await.unwrap();
    assert!(pending.has_dht());
    let np = Code::parse(pending.code()).unwrap().nameplate;
    assert!((1000..=9999).contains(&np), "{np}");
    pair_and_echo(&net.env, pending).await;
}

#[tokio::test]
async fn dht_ignores_stale_and_foreign_items() {
    let net = net();
    let np = 4321;
    // A stale sender session on our nameplate does not count as taken...
    plant(
        &net.env,
        np,
        SALT_SENDER_PAKE,
        [1; 16],
        now_secs() - ITEM_MAX_AGE_SECS - 60,
    )
    .await;
    // ...and a fresh receiver item from another session is not our receiver.
    plant(&net.env, np, SALT_RECEIVER_PAKE, [2; 16], now_secs()).await;
    force_nameplates(&net.env, &[np]);
    let pending = allocate_with(&dht_only(), &net.env).await.unwrap();
    assert_eq!(Code::parse(pending.code()).unwrap().nameplate, np);
    pair_and_echo(&net.env, pending).await;
}

#[tokio::test]
async fn dht_collision_picks_another_nameplate() {
    let net = net();
    plant(&net.env, 5555, SALT_SENDER_PAKE, [3; 16], now_secs()).await;
    force_nameplates(&net.env, &[5555, 6666]);
    let pending = allocate_with(&dht_only(), &net.env).await.unwrap();
    assert_eq!(Code::parse(pending.code()).unwrap().nameplate, 6666);
    assert!(pending.has_dht());
}

#[tokio::test]
async fn dht_wrong_code_aborts_both_sides() {
    let net = net();
    let pending = allocate_with(&dht_only(), &net.env).await.unwrap();
    let mut wrong = Code::parse(pending.code()).unwrap();
    wrong.words[1] = wordlist()
        .iter()
        .find(|w| **w != wrong.words[1])
        .unwrap()
        .to_string();
    let sender = tokio::spawn(pending.wait_for_peer(CancellationToken::new()));
    let r = tokio::time::timeout(
        T,
        join_with(
            &dht_only(),
            &wrong.to_string(),
            CancellationToken::new(),
            &net.env,
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(r, Err(PairingError::WrongCode)),
        "receiver: {:?}",
        r.err()
    );
    let s = tokio::time::timeout(T, sender).await.unwrap().unwrap();
    assert!(
        matches!(s, Err(PairingError::WrongCode)),
        "sender: {:?}",
        s.err()
    );
}

#[tokio::test]
async fn dht_cancel_stops_polling() {
    let net = net();
    let pending = allocate_with(&dht_only(), &net.env).await.unwrap();
    let gets = net.env.dht_gets.clone();
    let before = gets.load(Ordering::Relaxed);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(pending.wait_for_peer(cancel.clone()));
    // Wait until the sender is polling for a receiver.
    tokio::time::timeout(T, async {
        while gets.load(Ordering::Relaxed) < before + 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("sender never polled");
    cancel.cancel();
    let r = tokio::time::timeout(T, task).await.unwrap().unwrap();
    assert!(matches!(r, Err(PairingError::Cancelled)), "{:?}", r.err());
    let after_cancel = gets.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        gets.load(Ordering::Relaxed),
        after_cancel,
        "DHT polling continued after cancel"
    );
}
