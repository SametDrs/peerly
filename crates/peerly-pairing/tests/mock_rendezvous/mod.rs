//! Minimal in-test rendezvous server: WebSocket JSON + UDP probe on the same port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use peerly_proto::probe::{MAGIC, ProbeRequest, ProbeResponse};
use peerly_proto::rendezvous::{ClientMsg, ErrorCode, ServerMsg};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;

type Tx = UnboundedSender<ServerMsg>;

#[derive(Default)]
struct State {
    next: u32,
    /// nameplate -> [allocator, claimer]
    slots: HashMap<u32, [Option<Tx>; 2]>,
}

/// Starts the mock server; returns its `ws://` URL.
pub async fn start() -> String {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let udp = UdpSocket::bind(addr).await.unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        while let Ok((n, from)) = udp.recv_from(&mut buf).await {
            if let Ok(req) = peerly_proto::decode::<ProbeRequest>(&buf[..n])
                && req.magic == MAGIC
            {
                let resp = peerly_proto::encode(&ProbeResponse {
                    nonce: req.nonce,
                    observed: from,
                })
                .unwrap();
                let _ = udp.send_to(&resp, from).await;
            }
        }
    });
    let state = Arc::new(Mutex::new(State {
        next: 1,
        ..Default::default()
    }));
    tokio::spawn(async move {
        while let Ok((s, _)) = tcp.accept().await {
            tokio::spawn(client(s, state.clone()));
        }
    });
    format!("ws://{addr}")
}

async fn client(stream: TcpStream, state: Arc<Mutex<State>>) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink
                .send(Message::text(serde_json::to_string(&m).unwrap()))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let err = |code, message: &str| ServerMsg::Error {
        code,
        message: message.into(),
    };
    let mut joined: Option<(u32, usize)> = None;
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(t) = msg else { continue };
        let Ok(msg) = serde_json::from_str::<ClientMsg>(t.as_str()) else {
            break;
        };
        let reply = {
            let mut st = state.lock().unwrap();
            match msg {
                ClientMsg::Hello { version } => Some(ServerMsg::Welcome { version }),
                ClientMsg::Allocate => {
                    let n = st.next;
                    st.next += 1;
                    st.slots.insert(n, [Some(tx.clone()), None]);
                    joined = Some((n, 0));
                    Some(ServerMsg::Allocated { nameplate: n })
                }
                ClientMsg::Claim { nameplate } => match st.slots.get_mut(&nameplate) {
                    None => Some(err(ErrorCode::NameplateNotFound, "no such nameplate")),
                    Some(slot) if slot[1].is_some() => Some(err(ErrorCode::NameplateFull, "full")),
                    Some(slot) => {
                        slot[1] = Some(tx.clone());
                        joined = Some((nameplate, 1));
                        let _ = tx.send(ServerMsg::Claimed);
                        if let Some(a) = &slot[0] {
                            let _ = a.send(ServerMsg::PeerJoined);
                        }
                        None
                    }
                },
                ClientMsg::Relay { payload } => {
                    let other = joined
                        .and_then(|(n, side)| st.slots.get(&n).and_then(|s| s[1 - side].clone()));
                    match other {
                        Some(o) => {
                            let _ = o.send(ServerMsg::Peer { payload });
                            None
                        }
                        None => Some(err(ErrorCode::NoPeer, "no peer")),
                    }
                }
                ClientMsg::Close => break,
            }
        };
        if let Some(r) = reply {
            let _ = tx.send(r);
        }
    }
    if let Some((n, side)) = joined
        && let Some(slot) = state.lock().unwrap().slots.remove(&n)
        && let Some(o) = &slot[1 - side]
    {
        let _ = o.send(ServerMsg::PeerLeft);
    }
}
