//! THROWAWAY SPIKE (Aşama 0): ham QUIC ile aktarım hızı ölçümü.
//! Kullanım:
//!   spike-quic recv <port>
//!   spike-quic send <ip:port> <MB>
//! Güvenlik yok: sertifika doğrulaması kapalı. Sadece hız ölçmek için.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{Context, Result, bail};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig, VarInt};
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
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    // Yüksek bant genişliği için büyük pencereler.
    t.stream_receive_window(VarInt::from_u32(64 * 1024 * 1024));
    t.receive_window(VarInt::from_u32(128 * 1024 * 1024));
    t.send_window(128 * 1024 * 1024);
    Arc::new(t)
}

fn server_config() -> Result<ServerConfig> {
    let ck = rcgen::generate_simple_self_signed(vec!["peerly".into()])?;
    let cert = ck.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der());
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert], key.into())?;
    tls.alpn_protocols = vec![b"peerly-spike".to_vec()];
    let mut cfg = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

fn client_config() -> Result<ClientConfig> {
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(SkipVerify))
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"peerly-spike".to_vec()];
    let mut cfg = ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

fn mbps(bytes: u64, secs: f64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / secs
}

async fn recv(port: u16) -> Result<()> {
    let addr: SocketAddr = format!("0.0.0.0:{port}").parse()?;
    let ep = Endpoint::server(server_config()?, addr)?;
    println!("Alıcı dinliyor: {addr}");
    let conn = ep
        .accept()
        .await
        .context("bağlantı gelmedi")?
        .await?;
    println!("Bağlandı: {}", conn.remote_address());
    let mut stream = conn.accept_uni().await?;
    let start = Instant::now();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.read_chunk(usize::MAX, true).await? {
        total += chunk.bytes.len() as u64;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "Alındı: {:.1} MB, {:.2} sn, {:.1} MB/s",
        total as f64 / 1024.0 / 1024.0,
        secs,
        mbps(total, secs)
    );
    conn.close(0u32.into(), b"ok");
    ep.wait_idle().await;
    Ok(())
}

async fn send(addr: SocketAddr, mb: u64) -> Result<()> {
    let mut ep = Endpoint::client("0.0.0.0:0".parse()?)?;
    ep.set_default_client_config(client_config()?);
    let conn = ep.connect(addr, "peerly")?.await?;
    println!("Bağlandı: {}", conn.remote_address());
    let mut stream = conn.open_uni().await?;
    let buf = vec![0xABu8; 1024 * 1024];
    let start = Instant::now();
    for _ in 0..mb {
        stream.write_all(&buf).await?;
    }
    stream.finish()?;
    stream.stopped().await?;
    let secs = start.elapsed().as_secs_f64();
    let total = mb * 1024 * 1024;
    println!("Gönderildi: {mb} MB, {secs:.2} sn, {:.1} MB/s", mbps(total, secs));
    conn.close(0u32.into(), b"ok");
    ep.wait_idle().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("recv") => recv(args.get(2).context("port?")?.parse()?).await,
        Some("send") => {
            send(
                args.get(2).context("ip:port?")?.parse()?,
                args.get(3).context("MB?")?.parse()?,
            )
            .await
        }
        _ => bail!("kullanım: spike-quic recv <port> | send <ip:port> <MB>"),
    }
}
