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
            "--max-connections" => {
                cfg.max_connections = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-connections needs a number"))?
                    .parse()?;
            }
            "--max-connections-per-ip" => {
                cfg.max_connections_per_ip = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-connections-per-ip needs a number"))?
                    .parse()?;
            }
            "--hello-timeout-secs" => {
                let secs: u64 = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--hello-timeout-secs needs a number"))?
                    .parse()?;
                cfg.hello_timeout = std::time::Duration::from_secs(secs);
            }
            "--trust-proxy" => cfg.trust_proxy = true,
            "-h" | "--help" => {
                println!(
                    "usage: peerly-rendezvous [--listen 0.0.0.0:9009] [--max-connections 10000]\n\
                     \x20      [--max-connections-per-ip 32] [--hello-timeout-secs 10] [--trust-proxy]\n\n\
                     --trust-proxy: take the client IP from the right-most X-Forwarded-For entry.\n\
                     Only use behind a reverse proxy that sets the header."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    run(cfg).await
}
