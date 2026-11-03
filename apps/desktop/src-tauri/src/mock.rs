//! Phase 1 mock backend: emits a realistic `peerly_core::Event` sequence without any networking.
//! Mirrors the shape of `peerly_core::send` / `receive` closely enough to be swapped out.

use std::path::PathBuf;
use std::time::Duration;

use peerly_core::Event;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tokio_util::sync::CancellationToken;

const TOTAL_BYTES: u64 = 1_900_000_000;
const TOTAL_FILES: u32 = 312;
const DURATION_MS: u64 = 8_000;
const TICK_MS: u64 = 100;

async fn sleep_or_cancel(ms: u64, cancel: &CancellationToken) -> Result<(), String> {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
        _ = cancel.cancelled() => Err("cancelled".into()),
    }
}

async fn run_transfer(events: &UnboundedSender<Event>, cancel: &CancellationToken) -> Result<(), String> {
    let _ = events.send(Event::Started { total_bytes: TOTAL_BYTES, total_files: TOTAL_FILES });
    let ticks = DURATION_MS / TICK_MS;
    let per_tick = TOTAL_BYTES / ticks;
    let mut last_file = 0;
    for i in 1..=ticks {
        sleep_or_cancel(TICK_MS, cancel).await?;
        let done = if i == ticks { TOTAL_BYTES } else { per_tick * i };
        // Slightly wobbly speed around ~237 MB/s.
        let speed = 217_000_000 + ((i * 7919) % 40) * 1_000_000;
        let _ = events.send(Event::Progress { bytes_done: done, bytes_total: TOTAL_BYTES, bytes_per_sec: speed });
        let file_idx = (done * u64::from(TOTAL_FILES) / TOTAL_BYTES) as u32;
        if file_idx > last_file {
            last_file = file_idx;
            let _ = events.send(Event::FileCompleted { path: format!("photos/IMG_{file_idx:04}.jpg") });
        }
    }
    let _ = events.send(Event::Completed { files: TOTAL_FILES, bytes: TOTAL_BYTES, elapsed_ms: DURATION_MS });
    Ok(())
}

pub async fn send(
    _paths: Vec<PathBuf>,
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let _ = events.send(Event::Code { code: "42-river-candle".into() });
    let _ = events.send(Event::WaitingForPeer);
    sleep_or_cancel(3_000, &cancel).await?;
    let _ = events.send(Event::Connected { route: "direct" });
    sleep_or_cancel(1_000, &cancel).await?;
    run_transfer(&events, &cancel).await
}

pub async fn receive(
    events: UnboundedSender<Event>,
    cancel: CancellationToken,
    offer: oneshot::Receiver<bool>,
) -> Result<(), String> {
    let _ = events.send(Event::Connecting);
    sleep_or_cancel(1_500, &cancel).await?;
    let _ = events.send(Event::Connected { route: "direct" });
    let _ = events.send(Event::Offer { name: "photos".into(), files: TOTAL_FILES, total_bytes: TOTAL_BYTES });
    let accepted = tokio::select! {
        r = offer => r.unwrap_or(false),
        _ = cancel.cancelled() => return Err("cancelled".into()),
    };
    if !accepted {
        return Err("The transfer was declined.".into());
    }
    run_transfer(&events, &cancel).await
}
