//! Peerly rendezvous server.
//! Usage: `peerly-rendezvous [--listen 0.0.0.0:9009]` (WebSocket on TCP + UDP probe on the same port).

use peerly_rendezvous::{ServerConfig, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut cfg = ServerConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => {
                cfg.listen = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--listen needs an address"))?
                    .parse()?;
            }
            "-h" | "--help" => {
                println!("usage: peerly-rendezvous [--listen 0.0.0.0:9009]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    run(cfg).await
}
