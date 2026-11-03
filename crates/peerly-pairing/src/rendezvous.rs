//! Rendezvous client: JSON over WebSocket (`peerly_proto::rendezvous`).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use peerly_proto::PROTOCOL_VERSION;
use peerly_proto::pairing::PairingMsg;
use peerly_proto::rendezvous::{ClientMsg, ErrorCode, ServerMsg};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::PairingError;

/// Timeout for connecting + every request/response exchange with the server.
pub(crate) const RV_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

fn unreachable(e: impl std::fmt::Display) -> PairingError {
    PairingError::RendezvousUnreachable(e.to_string())
}

async fn with_timeout<T>(
    fut: impl Future<Output = Result<T, PairingError>>,
) -> Result<T, PairingError> {
    tokio::time::timeout(RV_TIMEOUT, fut)
        .await
        .map_err(|_| unreachable("rendezvous server did not answer in time"))?
}

fn map_server_error(code: ErrorCode, message: String) -> PairingError {
    match code {
        ErrorCode::NameplateNotFound => PairingError::CodeNotFound,
        ErrorCode::NameplateFull => PairingError::CodeInUse,
        ErrorCode::NoPeer => PairingError::PeerLeft,
        ErrorCode::VersionMismatch => {
            PairingError::Protocol(format!("rendezvous version mismatch: {message}"))
        }
        ErrorCode::BadRequest | ErrorCode::RateLimited => {
            PairingError::Protocol(format!("rendezvous error ({code:?}): {message}"))
        }
    }
}

impl Client {
    /// Connects and performs the `Hello`/`Welcome` handshake.
    pub(crate) async fn connect(url: &str) -> Result<Self, PairingError> {
        with_timeout(async {
            let (ws, _) = tokio_tungstenite::connect_async(url)
                .await
                .map_err(unreachable)?;
            let mut c = Client { ws };
            c.send(&ClientMsg::Hello {
                version: PROTOCOL_VERSION,
            })
            .await?;
            match c.recv().await? {
                ServerMsg::Welcome { version } if version == PROTOCOL_VERSION => Ok(c),
                ServerMsg::Welcome { version } => Err(PairingError::Protocol(format!(
                    "rendezvous speaks protocol v{version}, we speak v{PROTOCOL_VERSION}"
                ))),
                other => Err(PairingError::Protocol(format!(
                    "expected welcome, got {other:?}"
                ))),
            }
        })
        .await
    }

    async fn send(&mut self, msg: &ClientMsg) -> Result<(), PairingError> {
        let text = serde_json::to_string(msg).map_err(|e| PairingError::Protocol(e.to_string()))?;
        self.ws.send(Message::text(text)).await.map_err(unreachable)
    }

    /// Next server message. Server `Error`s are mapped to [`PairingError`].
    async fn recv(&mut self) -> Result<ServerMsg, PairingError> {
        loop {
            match self.ws.next().await {
                None | Some(Ok(Message::Close(_))) => {
                    return Err(unreachable("connection closed by server"));
                }
                Some(Err(e)) => return Err(unreachable(e)),
                Some(Ok(Message::Text(t))) => {
                    let msg: ServerMsg = serde_json::from_str(t.as_str()).map_err(|e| {
                        PairingError::Protocol(format!("bad rendezvous message: {e}"))
                    })?;
                    return match msg {
                        ServerMsg::Error { code, message } => Err(map_server_error(code, message)),
                        msg => Ok(msg),
                    };
                }
                Some(Ok(_)) => continue,
            }
        }
    }

    pub(crate) async fn allocate(&mut self) -> Result<u32, PairingError> {
        with_timeout(async {
            self.send(&ClientMsg::Allocate).await?;
            loop {
                if let ServerMsg::Allocated { nameplate } = self.recv().await? {
                    return Ok(nameplate);
                }
            }
        })
        .await
    }

    pub(crate) async fn claim(&mut self, nameplate: u32) -> Result<(), PairingError> {
        with_timeout(async {
            self.send(&ClientMsg::Claim { nameplate }).await?;
            loop {
                if let ServerMsg::Claimed = self.recv().await? {
                    return Ok(());
                }
            }
        })
        .await
    }

    /// Sender: waits (without timeout) until a receiver claims the nameplate.
    /// Returns a peer payload if one arrived before `PeerJoined`.
    pub(crate) async fn wait_peer_joined(&mut self) -> Result<Option<Vec<u8>>, PairingError> {
        loop {
            match self.recv().await? {
                ServerMsg::PeerJoined => return Ok(None),
                ServerMsg::Peer { payload } => return Ok(Some(payload)),
                ServerMsg::PeerLeft => return Err(PairingError::PeerLeft),
                _ => continue,
            }
        }
    }

    pub(crate) async fn relay(&mut self, msg: &PairingMsg) -> Result<(), PairingError> {
        let payload =
            peerly_proto::encode(msg).map_err(|e| PairingError::Protocol(e.to_string()))?;
        self.send(&ClientMsg::Relay { payload }).await
    }

    /// Next relayed payload from the peer.
    pub(crate) async fn next_peer_payload(&mut self) -> Result<Vec<u8>, PairingError> {
        loop {
            match self.recv().await? {
                ServerMsg::Peer { payload } => return Ok(payload),
                ServerMsg::PeerLeft => return Err(PairingError::PeerLeft),
                _ => continue,
            }
        }
    }

    /// Best-effort graceful close.
    pub(crate) async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            let _ = self.send(&ClientMsg::Close).await;
            let _ = self.ws.close(None).await;
        })
        .await;
    }
}
