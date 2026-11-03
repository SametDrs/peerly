//! Peerly wire protocol.
//!
//! This crate is the **contract** between every other crate (pairing, transfer,
//! rendezvous server). Changing a type here is a protocol change: bump
//! [`PROTOCOL_VERSION`] and update `docs/architecture.md`.
//!
//! Encodings:
//! - Rendezvous (client <-> server, WebSocket text frames): JSON, see [`rendezvous`].
//! - UDP address probe (client <-> server UDP): postcard, see [`probe`].
//! - Pairing payloads (peer <-> peer, tunnelled through the rendezvous `Relay`): postcard, see [`pairing`].
//! - Transfer (peer <-> peer over QUIC): length-prefixed postcard frames, see [`transfer`] and [`frame`].

pub const PROTOCOL_VERSION: u16 = 1;

/// QUIC ALPN identifier for peer-to-peer transfer connections.
pub const ALPN: &[u8] = b"peerly/1";

/// Hard upper bound for a single framed control message.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
}

/// Binary encoding used for every non-JSON message.
pub fn encode<T: serde::Serialize>(msg: &T) -> Result<Vec<u8>, CodecError> {
    Ok(postcard::to_stdvec(msg)?)
}

pub fn decode<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, CodecError> {
    Ok(postcard::from_bytes(bytes)?)
}

/// Rendezvous server protocol. JSON over WebSocket.
///
/// Flow:
/// 1. Both clients send [`ClientMsg::Hello`]; server answers [`ServerMsg::Welcome`].
/// 2. Sender sends [`ClientMsg::Allocate`] -> [`ServerMsg::Allocated`] with a fresh nameplate.
/// 3. Receiver sends [`ClientMsg::Claim`] -> [`ServerMsg::Claimed`]; sender gets [`ServerMsg::PeerJoined`].
/// 4. Both exchange opaque [`ClientMsg::Relay`] payloads, delivered to the other side as [`ServerMsg::Peer`].
/// 5. Either side sends [`ClientMsg::Close`] (or disconnects); the other gets [`ServerMsg::PeerLeft`].
///
/// A nameplate holds at most two clients, is single-use (released once either
/// side leaves) and expires if unclaimed for [`rendezvous::NAMEPLATE_TTL_SECS`].
pub mod rendezvous {
    use serde::{Deserialize, Serialize};

    pub const NAMEPLATE_TTL_SECS: u64 = 600;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ClientMsg {
        Hello { version: u16 },
        Allocate,
        Claim { nameplate: u32 },
        Relay { payload: Vec<u8> },
        Close,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ServerMsg {
        Welcome { version: u16 },
        Allocated { nameplate: u32 },
        Claimed,
        PeerJoined,
        Peer { payload: Vec<u8> },
        PeerLeft,
        Error { code: ErrorCode, message: String },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ErrorCode {
        VersionMismatch,
        NameplateNotFound,
        NameplateFull,
        NoPeer,
        BadRequest,
        RateLimited,
    }
}

/// UDP reflexive-address discovery ("what is my public ip:port?").
/// The rendezvous server listens for UDP on the **same port number** as its WebSocket listener.
pub mod probe {
    use serde::{Deserialize, Serialize};
    use std::net::SocketAddr;

    pub const MAGIC: [u8; 4] = *b"PRLY";

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ProbeRequest {
        pub magic: [u8; 4],
        pub nonce: [u8; 16],
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ProbeResponse {
        pub nonce: [u8; 16],
        /// Source address of the request as seen by the server.
        pub observed: SocketAddr,
    }
}

/// Peer-to-peer pairing messages, carried inside rendezvous `Relay` payloads.
/// The rendezvous server never sees plaintext [`PeerInfo`].
pub mod pairing {
    use serde::{Deserialize, Serialize};
    use std::net::SocketAddr;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PairingMsg {
        /// SPAKE2 message (symmetric mode), password = full code string.
        Pake(Vec<u8>),
        /// ChaCha20-Poly1305 sealed, postcard-encoded [`PeerInfo`]. Key derived
        /// from the SPAKE2 output via HKDF-SHA256. A decrypt failure means a wrong code.
        Sealed {
            nonce: [u8; 12],
            ciphertext: Vec<u8>,
        },
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PeerInfo {
        /// SHA-256 of the peer's self-signed QUIC certificate (DER). Pinned during the QUIC handshake.
        pub cert_fingerprint: [u8; 32],
        /// Addresses the peer can be reached on: LAN addresses + reflexive address from the probe.
        pub candidates: Vec<SocketAddr>,
    }
}

/// Serverless pairing on the local network.
///
/// The sender advertises an mDNS service [`lan::SERVICE_TYPE`] whose TXT record
/// carries `np=<nameplate>` and `v=<PROTOCOL_VERSION>`, and listens on TCP at the
/// advertised port. The receiver browses for a matching nameplate, connects, and
/// sends `frame(LanHello)`. After that, both sides exchange `frame(PairingMsg)`
/// in exactly the same order as over the rendezvous `Relay`.
pub mod lan {
    use serde::{Deserialize, Serialize};

    pub const SERVICE_TYPE: &str = "_peerly._tcp.local.";
    pub const TXT_NAMEPLATE: &str = "np";
    pub const TXT_VERSION: &str = "v";

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LanHello {
        pub version: u16,
        pub nameplate: u32,
    }
}

/// File transfer protocol over an established QUIC connection.
///
/// - Control: one bidirectional stream opened by the sender, framed [`Control`] messages.
///   sender -> `Offer`, receiver -> `Accept` | `Reject`, ... , receiver -> `Done`.
/// - Data: one unidirectional stream per file (several in parallel). Each stream is:
///   `frame(StreamHeader)` + exactly `size - offset` raw bytes + 32-byte BLAKE3 hash of the whole file.
pub mod transfer {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Manifest {
        /// Display name of what is being sent (file name, or folder name, or "N items").
        pub name: String,
        pub files: Vec<FileEntry>,
        /// Relative paths of empty directories to recreate.
        pub empty_dirs: Vec<String>,
        pub total_size: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FileEntry {
        pub index: u32,
        /// Relative path, `/`-separated, never absolute, never containing `..`.
        /// Receivers MUST re-validate this before touching the filesystem.
        pub path: String,
        pub size: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Control {
        Offer(Manifest),
        Accept,
        Reject,
        /// Receiver -> sender once every file is written and verified.
        Done,
        /// Either side aborts.
        Cancel {
            reason: String,
        },
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct StreamHeader {
        pub index: u32,
        /// Always 0 in v1. Reserved for resume.
        pub offset: u64,
    }
}

/// Length-prefixed framing: 4-byte big-endian length, then postcard bytes.
pub mod frame {
    use super::{CodecError, MAX_FRAME_LEN};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), CodecError>
    where
        W: AsyncWrite + Unpin,
        T: serde::Serialize,
    {
        let bytes = super::encode(msg)?;
        if bytes.len() > MAX_FRAME_LEN {
            return Err(CodecError::FrameTooLarge(bytes.len()));
        }
        w.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
        w.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn read_frame<R, T>(r: &mut R) -> Result<T, CodecError>
    where
        R: AsyncRead + Unpin,
        T: serde::de::DeserializeOwned,
    {
        let mut len = [0u8; 4];
        r.read_exact(&mut len).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME_LEN {
            return Err(CodecError::FrameTooLarge(len));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        Ok(postcard::from_bytes(&buf)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transfer::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let msg = Control::Offer(Manifest {
            name: "photos".into(),
            files: vec![FileEntry {
                index: 0,
                path: "photos/a.jpg".into(),
                size: 42,
            }],
            empty_dirs: vec![],
            total_size: 42,
        });
        let mut buf = Vec::new();
        frame::write_frame(&mut buf, &msg).await.unwrap();
        let back: Control = frame::read_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn rendezvous_json_shape() {
        let j = serde_json::to_string(&rendezvous::ClientMsg::Claim { nameplate: 7 }).unwrap();
        assert_eq!(j, r#"{"type":"claim","nameplate":7}"#);
    }
}
