//! QUIC setup: per-session self-signed certificates, fingerprint pinning on
//! both sides (mutual TLS without a CA), hole punching and candidate racing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use peerly_proto::pairing::PeerInfo;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, EndpointConfig, TokioRuntime, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CertificateError, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::net::{self, Local};
use crate::{Paired, PairingError, Route};

/// Overall budget for hole punching + handshake.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Delay between starting connection attempts to successive candidates.
const STAGGER: Duration = Duration::from_millis(150);
const PUNCH_INTERVAL: Duration = Duration::from_millis(250);
/// Default punching time on the sender (rendezvous / LAN pairing, where both
/// sides learn each other's candidates within milliseconds).
const PUNCH_TIME: Duration = Duration::from_secs(6);

/// How long the sender (QUIC server) accepts handshakes and punches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AcceptWindow {
    pub total: Duration,
    pub punch: Duration,
}

impl AcceptWindow {
    /// Both sides finish the exchange at about the same moment.
    pub(crate) const DIRECT: AcceptWindow = AcceptWindow {
        total: CONNECT_TIMEOUT,
        punch: PUNCH_TIME,
    };

    /// DHT pairing: the sender finishes the exchange first; the receiver may
    /// only see the sender's sealed info up to one exchange step later
    /// (its poll is bounded by `skew`). The sender keeps accepting and punching
    /// until the receiver's full connect window has passed.
    pub(crate) const fn lagging_receiver(skew: Duration) -> AcceptWindow {
        let total = Duration::from_secs(CONNECT_TIMEOUT.as_secs() + skew.as_secs());
        AcceptWindow {
            total,
            punch: total,
        }
    }
}
/// Server name used in the handshake. Ignored by the pinning verifier.
const SERVER_NAME: &str = "peerly";
/// Byte the receiver writes on a uni stream to tell the sender which of
/// possibly several parallel connections it picked.
const SELECTED: u8 = 1;

fn tls_err(e: impl std::fmt::Display) -> PairingError {
    PairingError::Protocol(format!("tls setup: {e}"))
}

/// SHA-256 of a DER certificate.
pub fn fingerprint(cert_der: &[u8]) -> [u8; 32] {
    Sha256::digest(cert_der).into()
}

/// A per-session self-signed certificate and its key.
pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: [u8; 32],
}

impl Identity {
    pub fn generate() -> Result<Self, PairingError> {
        let ck = rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).map_err(tls_err)?;
        let cert = ck.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der());
        let fingerprint = fingerprint(&cert);
        Ok(Identity {
            cert,
            key,
            fingerprint,
        })
    }

    fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

/// Transport tuned for throughput (large windows), with keep-alive and idle timeout.
pub fn transport_config() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.stream_receive_window(VarInt::from_u32(64 * 1024 * 1024));
    t.receive_window(VarInt::from_u32(128 * 1024 * 1024));
    // Kept moderate: the transfer layer counts progress as bytes handed to quinn,
    // so a huge send window would make progress run far ahead of delivery.
    // 32 MiB still covers ~1 Gbit/s at ~250 ms RTT.
    t.send_window(32 * 1024 * 1024);
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(VarInt::from_u32(30_000).into()));
    Arc::new(t)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts exactly one certificate (by SHA-256 fingerprint). Handshake
/// signatures are still verified for real; only the CA chain is replaced.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl Pinned {
    fn check(&self, end_entity: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if fingerprint(end_entity) == self.fingerprint {
            Ok(())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|_| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for Pinned {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|_| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Sender (QUIC server) config: presents `me`, requires a client cert with `peer_fingerprint`.
pub fn server_config(
    me: &Identity,
    peer_fingerprint: [u8; 32],
) -> Result<quinn::ServerConfig, PairingError> {
    let provider = provider();
    let verifier = Arc::new(Pinned {
        fingerprint: peer_fingerprint,
        algs: provider.signature_verification_algorithms,
    });
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_err)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![me.cert.clone()], me.key())
        .map_err(tls_err)?;
    tls.alpn_protocols = vec![peerly_proto::ALPN.to_vec()];
    let crypto = QuicServerConfig::try_from(tls).map_err(tls_err)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}

/// Receiver (QUIC client) config: presents `me`, requires a server cert with `peer_fingerprint`.
pub fn client_config(
    me: &Identity,
    peer_fingerprint: [u8; 32],
) -> Result<quinn::ClientConfig, PairingError> {
    let provider = provider();
    let verifier = Arc::new(Pinned {
        fingerprint: peer_fingerprint,
        algs: provider.signature_verification_algorithms,
    });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_err)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![me.cert.clone()], me.key())
        .map_err(tls_err)?;
    tls.alpn_protocols = vec![peerly_proto::ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(tls).map_err(tls_err)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}

fn route_for(addr: SocketAddr) -> Route {
    if net::is_lan(addr.ip()) {
        Route::Lan
    } else {
        Route::Internet
    }
}

/// Aborts the task when dropped (e.g. on cancellation).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Small datagrams to every candidate so our NAT/firewall lets their packets in.
/// A single zero byte is not a valid QUIC packet and is silently dropped by quinn.
async fn punch(sock: std::net::UdpSocket, targets: Vec<SocketAddr>, time: Duration) {
    let mut tick = tokio::time::interval(PUNCH_INTERVAL);
    let rounds = (time.as_millis() / PUNCH_INTERVAL.as_millis()).max(1);
    for _ in 0..rounds {
        tick.tick().await;
        for t in &targets {
            let _ = sock.send_to(&[0u8], t);
        }
    }
}

/// Sender side: QUIC server on the probed socket.
pub(crate) async fn accept(
    local: Local,
    peer: PeerInfo,
    window: AcceptWindow,
) -> Result<Paired, PairingError> {
    let targets = net::sanitize(peer.candidates);
    let server_cfg = server_config(&local.identity, peer.cert_fingerprint)?;
    let punch_sock = local.socket.try_clone()?;
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(server_cfg),
        local.socket,
        Arc::new(TokioRuntime),
    )?;
    let _punch = AbortOnDrop(tokio::spawn(punch(punch_sock, targets, window.punch)));

    let connection = tokio::time::timeout(window.total, accept_selected(&endpoint))
        .await
        .ok()
        .flatten()
        .ok_or(PairingError::PeerUnreachable)?;
    let peer_addr = net::normalize(connection.remote_address());
    Ok(Paired {
        connection,
        endpoint,
        peer_addr,
        route: route_for(peer_addr),
    })
}

/// Accepts handshakes until the receiver marks one as selected; the rest are dropped.
async fn accept_selected(endpoint: &Endpoint) -> Option<Connection> {
    let mut pending = JoinSet::new();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let incoming = incoming?;
                pending.spawn(async move {
                    let conn = incoming.await.ok()?;
                    let mut s = conn.accept_uni().await.ok()?;
                    let mut b = [0u8; 1];
                    s.read_exact(&mut b).await.ok()?;
                    (b[0] == SELECTED).then_some(conn)
                });
            }
            Some(res) = pending.join_next(), if !pending.is_empty() => {
                if let Ok(Some(conn)) = res {
                    return Some(conn);
                }
            }
        }
    }
}

/// Receiver side: QUIC client racing all candidates (LAN first, staggered).
pub(crate) async fn connect(local: Local, peer: PeerInfo) -> Result<Paired, PairingError> {
    let targets = net::sanitize(peer.candidates);
    let client_cfg = client_config(&local.identity, peer.cert_fingerprint)?;
    let mut endpoint = Endpoint::new(
        EndpointConfig::default(),
        None,
        local.socket,
        Arc::new(TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_cfg);

    let race = async {
        let mut set = JoinSet::new();
        for (i, addr) in targets.into_iter().enumerate() {
            let ep = endpoint.clone();
            set.spawn(async move {
                tokio::time::sleep(STAGGER * i as u32).await;
                let conn = ep.connect(addr, SERVER_NAME).ok()?.await;
                match conn {
                    Ok(c) => Some((c, addr)),
                    Err(e) => {
                        tracing::debug!(%addr, "candidate failed: {e}");
                        None
                    }
                }
            });
        }
        while let Some(res) = set.join_next().await {
            if let Ok(Some(won)) = res {
                return Some(won);
            }
        }
        None
    };
    let (connection, addr) = tokio::time::timeout(CONNECT_TIMEOUT, race)
        .await
        .ok()
        .flatten()
        .ok_or(PairingError::PeerUnreachable)?;

    let mut s = connection
        .open_uni()
        .await
        .map_err(|_| PairingError::PeerUnreachable)?;
    s.write_all(&[SELECTED])
        .await
        .map_err(|_| PairingError::PeerUnreachable)?;
    s.finish().map_err(|_| PairingError::PeerUnreachable)?;
    Ok(Paired {
        connection,
        endpoint,
        peer_addr: addr,
        route: route_for(addr),
    })
}
