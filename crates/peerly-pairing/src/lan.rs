//! Serverless pairing on the local network (`peerly_proto::lan`).
//!
//! Sender: [`LanListener`] = TCP listener + optional mDNS advertisement; both
//! are torn down when it is dropped. Receiver: connects over TCP, sends
//! `frame(LanHello)`, then both sides run the usual exchange over [`LanMailbox`].
//! Discovery lives in `mdns.rs`; everything here works with plain addresses.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use peerly_proto::lan::{LanHello, MAX_PAIRING_FRAME_LEN};
use peerly_proto::pairing::{PairingMsg, PeerInfo};
use peerly_proto::{CodecError, PROTOCOL_VERSION, frame};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::code::Code;
use crate::mailbox::Mailbox;
use crate::net::{Discovery, Local};
use crate::seal::Role;
use crate::{PairingError, exchange, mdns};

/// Failed SPAKE/decrypt attempts the sender tolerates on the LAN (several
/// senders may share a 1..=999 nameplate); one more aborts with `WrongCode`.
pub(crate) const MAX_WRONG_CODES: u32 = 3;
/// How long the receiver browses mDNS for the sender.
pub(crate) const BROWSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Receiver: TCP connect budget per discovered sender.
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Sender: time a new connection gets to send its `LanHello`.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Sender: concurrent unauthenticated connections; extra ones are dropped.
const MAX_PENDING: usize = 8;

fn codec_err(e: CodecError) -> PairingError {
    match e {
        CodecError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            PairingError::PeerLeft
        }
        CodecError::Io(io) => PairingError::Io(io),
        other => PairingError::Protocol(format!("bad LAN pairing message: {other}")),
    }
}

/// [`Mailbox`] over a direct TCP connection: `frame(PairingMsg)` both ways.
pub(crate) struct LanMailbox {
    stream: TcpStream,
}

impl LanMailbox {
    pub(crate) fn new(stream: TcpStream) -> Self {
        let _ = stream.set_nodelay(true);
        LanMailbox { stream }
    }
}

impl Mailbox for LanMailbox {
    async fn send(&mut self, msg: &PairingMsg) -> Result<(), PairingError> {
        frame::write_frame(&mut self.stream, msg)
            .await
            .map_err(codec_err)
    }

    async fn recv(&mut self) -> Result<PairingMsg, PairingError> {
        // Every LAN frame is unauthenticated input: cap it before allocating.
        frame::read_frame_limited(&mut self.stream, MAX_PAIRING_FRAME_LEN)
            .await
            .map_err(codec_err)
    }

    async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), self.stream.shutdown()).await;
    }
}

/// Runs the exchange over `mb` and closes it either way.
async fn exchange_and_close(
    mut mb: LanMailbox,
    code: &str,
    role: Role,
) -> Result<(Local, PeerInfo), PairingError> {
    let r = exchange(&mut mb, code, role, Discovery::LanOnly).await;
    mb.close().await;
    r
}

/// Sender side: TCP listener plus (optionally) its mDNS advertisement.
/// Dropping it closes the listener and removes the advertisement.
pub(crate) struct LanListener {
    tcp: TcpListener,
    nameplate: u32,
    _advertisement: Option<mdns::Advertisement>,
}

impl LanListener {
    pub(crate) async fn bind(nameplate: u32, advertise: bool) -> Result<Self, PairingError> {
        let tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        let port = tcp.local_addr()?.port();
        let advertisement = if advertise {
            Some(mdns::Advertisement::start(nameplate, port)?)
        } else {
            None
        };
        Ok(LanListener {
            tcp,
            nameplate,
            _advertisement: advertisement,
        })
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.tcp.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// Accepts receivers until one completes the exchange with the right code.
    /// Up to [`MAX_WRONG_CODES`] wrong-code attempts are tolerated; other
    /// failed connections (bad hello, disconnects, garbage) are just dropped.
    pub(crate) async fn serve(&self, code: &str) -> Result<(Local, PeerInfo), PairingError> {
        let mut attempts = FuturesUnordered::new();
        let mut wrong = 0u32;
        loop {
            tokio::select! {
                acc = self.tcp.accept() => match acc {
                    Ok((stream, from)) => {
                        if attempts.len() >= MAX_PENDING {
                            tracing::debug!(%from, "too many pending LAN connections; dropping");
                            continue;
                        }
                        tracing::debug!(%from, "LAN connection");
                        attempts.push(self.attempt(stream, code));
                    }
                    Err(e) => {
                        tracing::warn!("LAN accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                Some(r) = attempts.next(), if !attempts.is_empty() => match r {
                    Ok(done) => return Ok(done),
                    Err(PairingError::WrongCode) => {
                        wrong += 1;
                        tracing::warn!(wrong, "LAN pairing attempt with a wrong code");
                        if wrong > MAX_WRONG_CODES {
                            return Err(PairingError::WrongCode);
                        }
                    }
                    Err(e) => tracing::debug!("LAN pairing attempt failed: {e}"),
                },
            }
        }
    }

    async fn attempt(
        &self,
        stream: TcpStream,
        code: &str,
    ) -> Result<(Local, PeerInfo), PairingError> {
        let mut mb = LanMailbox::new(stream);
        // Small frame cap: the length prefix is rejected before any allocation.
        let hello: LanHello = tokio::time::timeout(
            HELLO_TIMEOUT,
            frame::read_frame_limited(&mut mb.stream, MAX_PAIRING_FRAME_LEN),
        )
        .await
        .map_err(|_| PairingError::Timeout)?
        .map_err(codec_err)?;
        if hello.version != PROTOCOL_VERSION || hello.nameplate != self.nameplate {
            return Err(PairingError::Protocol(format!(
                "LAN hello for v{} nameplate {}",
                hello.version, hello.nameplate
            )));
        }
        exchange_and_close(mb, code, Role::Sender).await
    }
}

/// Receiver side: connects to one sender (any of its addresses, in parallel),
/// says hello and runs the exchange.
pub(crate) async fn exchange_at(
    code: &Code,
    addrs: &[SocketAddr],
) -> Result<(Local, PeerInfo), PairingError> {
    let stream = tokio::time::timeout(TCP_CONNECT_TIMEOUT, async {
        let mut tries: FuturesUnordered<_> = addrs.iter().map(|a| TcpStream::connect(*a)).collect();
        let mut last = None;
        while let Some(r) = tries.next().await {
            match r {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no address to connect to")))
    })
    .await
    .map_err(|_| PairingError::PeerUnreachable)??;
    let mut mb = LanMailbox::new(stream);
    frame::write_frame(
        &mut mb.stream,
        &LanHello {
            version: PROTOCOL_VERSION,
            nameplate: code.nameplate,
        },
    )
    .await
    .map_err(codec_err)?;
    exchange_and_close(mb, &code.to_string(), Role::Receiver).await
}

/// Receiver side: browses mDNS for the code's nameplate and tries each match
/// in turn. `CodeNotFound` if nothing answered; otherwise the last failure
/// (e.g. `WrongCode` when every match rejected the code).
pub(crate) async fn discover_and_exchange(code: &Code) -> Result<(Local, PeerInfo), PairingError> {
    let mut browser = match mdns::Browser::start(BROWSE_TIMEOUT) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("LAN discovery unavailable: {e}");
            return Err(PairingError::CodeNotFound);
        }
    };
    let mut last = PairingError::CodeNotFound;
    while let Some(addrs) = browser.next_match(code.nameplate).await {
        match exchange_at(code, &addrs).await {
            Ok(done) => return Ok(done),
            Err(e) => {
                tracing::debug!(?addrs, "LAN sender did not pair: {e}");
                // A wrong code is the most useful thing to report.
                if !matches!(last, PairingError::WrongCode) {
                    last = e;
                }
            }
        }
    }
    Err(last)
}
