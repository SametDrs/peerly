//! Peerly high-level API. The only crate the CLI and the desktop app talk to.
//! Glues `peerly-pairing` (code -> connection) and `peerly-transfer` (files over the connection),
//! and turns their events into one UI-friendly [`Event`] stream (serde-serializable for Tauri).
//!
//! OWNER: integration agent (phase 2). Signatures are the contract for the CLI and the desktop app.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub use peerly_pairing::Route;
pub use peerly_proto::transfer::Manifest;
use peerly_transfer::TransferEvent;

/// Address of a locally run rendezvous server (`peerly-rendezvous` default port).
/// Not used unless configured: the rendezvous server is optional.
pub const DEFAULT_RENDEZVOUS: &str = "ws://127.0.0.1:9009";

#[derive(Debug, Clone)]
pub struct Config {
    /// Optional rendezvous server for pairing across the internet.
    pub rendezvous_url: Option<String>,
    /// Serverless pairing on the local network (mDNS).
    pub lan: bool,
    /// Serverless pairing over the internet (Mainline DHT + STUN).
    pub dht: bool,
}

impl Default for Config {
    /// Serverless: LAN + DHT, no rendezvous server.
    fn default() -> Self {
        Self {
            rendezvous_url: None,
            lan: true,
            dht: true,
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(std::env::var(name).as_deref().map(str::trim), Ok("1"))
}

impl Config {
    /// `PEERLY_RENDEZVOUS` set (non-empty) enables the rendezvous server;
    /// `PEERLY_NO_LAN=1` disables LAN pairing, `PEERLY_NO_DHT=1` DHT pairing.
    pub fn from_env() -> Self {
        Self {
            rendezvous_url: std::env::var("PEERLY_RENDEZVOUS")
                .ok()
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty()),
            lan: !env_flag("PEERLY_NO_LAN"),
            dht: !env_flag("PEERLY_NO_DHT"),
        }
    }

    fn pairing(&self) -> peerly_pairing::Config {
        peerly_pairing::Config {
            rendezvous_url: self.rendezvous_url.clone(),
            lan: self.lan,
            dht: self.dht,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// Sender: show this code to the user.
    Code {
        code: String,
    },
    WaitingForPeer,
    Connecting,
    Connected {
        route: &'static str,
    },
    /// Receiver: offer received, waiting for the accept decision.
    Offer {
        name: String,
        files: u32,
        total_bytes: u64,
    },
    Started {
        total_bytes: u64,
        total_files: u32,
    },
    Progress {
        bytes_done: u64,
        bytes_total: u64,
        bytes_per_sec: u64,
    },
    FileCompleted {
        path: String,
    },
    Completed {
        files: u32,
        bytes: u64,
        elapsed_ms: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Pairing(#[from] peerly_pairing::PairingError),
    #[error(transparent)]
    Transfer(#[from] peerly_transfer::TransferError),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    fn normalize(self) -> Self {
        match self {
            Error::Pairing(peerly_pairing::PairingError::Cancelled)
            | Error::Transfer(peerly_transfer::TransferError::Cancelled) => Error::Cancelled,
            e => e,
        }
    }
}

fn route_str(r: Route) -> &'static str {
    match r {
        Route::Lan => "lan",
        Route::Internet => "internet",
    }
}

/// Smooths the instantaneous transfer rate with an exponential moving average.
struct RateMeter {
    last: Option<(Instant, u64)>,
    ema: f64,
}

impl RateMeter {
    fn new() -> Self {
        Self {
            last: None,
            ema: 0.0,
        }
    }
    fn update(&mut self, bytes_done: u64) -> u64 {
        let now = Instant::now();
        match self.last {
            None => self.last = Some((now, bytes_done)),
            Some((t, b)) => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt >= 0.25 {
                    let inst = bytes_done.saturating_sub(b) as f64 / dt;
                    self.ema = if self.ema == 0.0 {
                        inst
                    } else {
                        0.2 * inst + 0.8 * self.ema
                    };
                    self.last = Some((now, bytes_done));
                }
            }
        }
        self.ema as u64
    }
}

/// Forwards transfer events as core events until the channel closes.
fn spawn_forwarder(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<TransferEvent>,
    events: UnboundedSender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut meter = RateMeter::new();
        while let Some(ev) = rx.recv().await {
            let out = match ev {
                TransferEvent::Offered(_) => continue,
                TransferEvent::Started {
                    total_bytes,
                    total_files,
                } => Event::Started {
                    total_bytes,
                    total_files,
                },
                TransferEvent::Progress {
                    bytes_done,
                    bytes_total,
                } => Event::Progress {
                    bytes_done,
                    bytes_total,
                    bytes_per_sec: meter.update(bytes_done),
                },
                TransferEvent::FileCompleted { path } => Event::FileCompleted { path },
            };
            let _ = events.send(out);
        }
    })
}

async fn wait_idle(endpoint: &quinn::Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
}

pub async fn send(
    cfg: &Config,
    paths: Vec<PathBuf>,
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<(), Error> {
    let pcfg = cfg.pairing();
    // Allocation can take seconds (DHT lookup), so it must honour cancel too;
    // otherwise a cancelled send would still pop up a code afterwards.
    let pending = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        r = peerly_pairing::allocate(&pcfg) => r?,
    };
    let _ = events.send(Event::Code {
        code: pending.code().to_string(),
    });
    let _ = events.send(Event::WaitingForPeer);
    let paired = pending
        .wait_for_peer(cancel.clone())
        .await
        .map_err(Error::from)
        .map_err(Error::normalize)?;
    let _ = events.send(Event::Connected {
        route: route_str(paired.route),
    });

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let fwd = spawn_forwarder(rx, events.clone());
    let res = peerly_transfer::send(&paired.connection, &paths, tx, cancel).await;
    let _ = fwd.await;
    wait_idle(&paired.endpoint).await;
    let summary = res.map_err(Error::from).map_err(Error::normalize)?;
    let _ = events.send(Event::Completed {
        files: summary.files,
        bytes: summary.bytes,
        elapsed_ms: summary.elapsed.as_millis() as u64,
    });
    Ok(())
}

pub async fn receive<F, Fut>(
    cfg: &Config,
    code: &str,
    dest_dir: &Path,
    decide: F,
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<(), Error>
where
    F: FnOnce(Manifest) -> Fut + Send,
    Fut: Future<Output = bool> + Send,
{
    let pcfg = cfg.pairing();
    let _ = events.send(Event::Connecting);
    let paired = peerly_pairing::join(&pcfg, code, cancel.clone())
        .await
        .map_err(Error::from)
        .map_err(Error::normalize)?;
    let _ = events.send(Event::Connected {
        route: route_str(paired.route),
    });

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let fwd = spawn_forwarder(rx, events.clone());
    let ev2 = events.clone();
    let wrapped = move |m: Manifest| async move {
        let _ = ev2.send(Event::Offer {
            name: m.name.clone(),
            files: m.files.len() as u32,
            total_bytes: m.total_size,
        });
        decide(m).await
    };
    let res = peerly_transfer::receive(&paired.connection, dest_dir, wrapped, tx, cancel).await;
    let _ = fwd.await;
    wait_idle(&paired.endpoint).await;
    match res {
        Ok(summary) => {
            let _ = events.send(Event::Completed {
                files: summary.files,
                bytes: summary.bytes,
                elapsed_ms: summary.elapsed.as_millis() as u64,
            });
            Ok(())
        }
        Err(peerly_transfer::TransferError::Rejected) => Ok(()),
        Err(e) => Err(Error::from(e).normalize()),
    }
}
