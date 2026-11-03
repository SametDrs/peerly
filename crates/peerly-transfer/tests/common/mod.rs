//! Test helpers: a connected quinn pair on loopback with a self-signed cert.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

#[derive(Debug)]
struct SkipVerify;

impl ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.stream_receive_window(VarInt::from_u32(64 * 1024 * 1024));
    t.receive_window(VarInt::from_u32(128 * 1024 * 1024));
    t.send_window(128 * 1024 * 1024);
    Arc::new(t)
}

pub struct Pair {
    /// Sender side (QUIC server role).
    pub sender: Connection,
    /// Receiver side (QUIC client role).
    pub receiver: Connection,
    _eps: (Endpoint, Endpoint),
}

pub async fn pair() -> Pair {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let ck = rcgen::generate_simple_self_signed(vec!["peerly".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der());
    let mut stls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![ck.cert.der().clone()], key.into())
        .unwrap();
    stls.alpn_protocols = vec![peerly_proto::ALPN.to_vec()];
    let mut scfg = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(stls).unwrap()));
    scfg.transport_config(transport());
    let server = Endpoint::server(scfg, "127.0.0.1:0".parse().unwrap()).unwrap();

    let mut ctls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify))
        .with_no_client_auth();
    ctls.alpn_protocols = vec![peerly_proto::ALPN.to_vec()];
    let mut ccfg = ClientConfig::new(Arc::new(QuicClientConfig::try_from(ctls).unwrap()));
    ccfg.transport_config(transport());
    let mut client = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(ccfg);

    let addr = server.local_addr().unwrap();
    let (s, c) = tokio::join!(async { server.accept().await.unwrap().await.unwrap() }, async {
        client.connect(addr, "peerly").unwrap().await.unwrap()
    });
    Pair { sender: s, receiver: c, _eps: (server, client) }
}

/// Deterministic pseudo-random bytes (xorshift), fast enough for tens of MB.
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Relative `/` path -> `Some(contents)` for files, `None` for directories.
pub fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p: PathBuf = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, None);
                walk(root, &p, out);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
