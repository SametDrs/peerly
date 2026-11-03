//! Peerly CLI: `peerly send <paths...>` / `peerly receive <code>`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use peerly_core::{Config, Error, Event};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "peerly", version, about = "Direct peer-to-peer file transfer")]
struct Cli {
    /// Rendezvous server URL (overrides PEERLY_RENDEZVOUS).
    #[arg(long, global = true)]
    rendezvous: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send files or folders; prints a code for the receiver.
    Send {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Receive using a code from the sender.
    Receive {
        code: String,
        /// Destination directory (default: current directory).
        #[arg(long, default_value = ".")]
        to: PathBuf,
        /// Accept without asking.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn print_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut bar: Option<ProgressBar> = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::Code { code } => {
                    eprintln!("\n  Your code:  {code}\n");
                    eprintln!("  On the other device run:  peerly receive {code}\n");
                }
                Event::WaitingForPeer => eprintln!("Waiting for the receiver..."),
                Event::Connecting => eprintln!("Connecting..."),
                Event::Connected { route } => eprintln!("Connected ({route})."),
                Event::Offer { .. } => {}
                Event::Started { total_bytes, .. } => {
                    let b = ProgressBar::new(total_bytes);
                    b.set_style(
                        ProgressStyle::with_template(
                            "{percent:>3}% [{bar:30}] {bytes_per_sec} ETA {eta}",
                        )
                        .unwrap()
                        .progress_chars("=> "),
                    );
                    bar = Some(b);
                }
                Event::Progress { bytes_done, .. } => {
                    if let Some(b) = &bar {
                        b.set_position(bytes_done);
                    }
                }
                Event::FileCompleted { .. } => {}
                Event::Completed {
                    files,
                    bytes,
                    elapsed_ms,
                } => {
                    if let Some(b) = bar.take() {
                        b.finish_and_clear();
                    }
                    eprintln!(
                        "Done: {files} file(s), {} in {:.1}s.",
                        human(bytes),
                        elapsed_ms as f64 / 1000.0
                    );
                }
            }
        }
    })
}

fn friendly(e: &Error) -> String {
    use peerly_core::Error::*;
    match e {
        Cancelled => "Cancelled.".into(),
        e => format!("Error: {e}"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut cfg = Config::from_env();
    if let Some(u) = cli.rendezvous {
        cfg.rendezvous_url = Some(u);
    }
    let cancel = CancellationToken::new();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
    }
    let (tx, rx) = unbounded_channel();
    let printer = print_events(rx);

    let res = match cli.cmd {
        Cmd::Send { paths } => peerly_core::send(&cfg, paths, tx, cancel).await,
        Cmd::Receive { code, to, yes } => {
            let decide = move |m: peerly_core::Manifest| async move {
                if yes {
                    return true;
                }
                let prompt = format!(
                    "Accept \"{}\" ({} files, {})? [y/N] ",
                    m.name,
                    m.files.len(),
                    human(m.total_size)
                );
                tokio::task::spawn_blocking(move || {
                    eprint!("{prompt}");
                    let _ = std::io::stderr().flush();
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).is_ok()
                        && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
                })
                .await
                .unwrap_or(false)
            };
            peerly_core::receive(&cfg, &code, &to, decide, tx, cancel).await
        }
    };
    let _ = printer.await;
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", friendly(&e));
            ExitCode::FAILURE
        }
    }
}
