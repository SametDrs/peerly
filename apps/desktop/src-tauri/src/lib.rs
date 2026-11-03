use std::path::{Path, PathBuf};
use std::sync::Mutex;

use peerly_core::Event;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const EVENT_NAME: &str = "peerly://event";

/// Shared state: the running task's cancel token and the pending receiver-side offer answer.
#[derive(Default)]
struct AppState {
    task: Mutex<Option<CancellationToken>>,
    offer: Mutex<Option<oneshot::Sender<bool>>>,
}

fn emit_error(app: &AppHandle, code: &str, message: &str) {
    let _ = app.emit(
        EVENT_NAME,
        serde_json::json!({ "kind": "error", "code": code, "message": message }),
    );
}

/// Stable error code for the UI (translated there).
fn error_code(e: &peerly_core::Error) -> &'static str {
    use peerly_core::Error as E;
    use peerly_pairing::PairingError as P;
    use peerly_transfer::TransferError as T;
    match e {
        E::Pairing(P::RendezvousUnreachable(_)) => "rendezvous_unreachable",
        E::Pairing(P::CodeNotFound) => "code_not_found",
        E::Pairing(P::CodeInUse) => "code_in_use",
        E::Pairing(P::WrongCode) => "wrong_code",
        E::Pairing(P::InvalidCode) => "invalid_code",
        E::Pairing(P::PeerUnreachable) => "peer_unreachable",
        E::Pairing(P::PeerLeft) => "peer_left",
        E::Pairing(P::Timeout) => "timeout",
        E::Transfer(T::Rejected) => "rejected",
        E::Transfer(T::PeerCancelled(_)) => "peer_cancelled",
        E::Transfer(T::DiskFull { .. }) => "disk_full",
        E::Transfer(T::HashMismatch(_)) => "hash_mismatch",
        E::Transfer(T::Connection(_)) => "peer_left",
        _ => "other",
    }
}

/// Registers a new task token (cancelling any previous one) and forwards core events to the webview.
fn begin_task(
    app: &AppHandle,
    state: &AppState,
) -> (mpsc::UnboundedSender<Event>, CancellationToken) {
    let cancel = CancellationToken::new();
    if let Some(old) = state.task.lock().unwrap().replace(cancel.clone()) {
        old.cancel();
    }
    state.offer.lock().unwrap().take();
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app.emit(EVENT_NAME, &ev);
        }
    });
    (tx, cancel)
}

fn finish(app: &AppHandle, cancel: &CancellationToken, result: Result<(), peerly_core::Error>) {
    if let Err(e) = result
        && !matches!(e, peerly_core::Error::Cancelled)
        && !cancel.is_cancelled()
    {
        emit_error(app, error_code(&e), &e.to_string());
    }
}

#[tauri::command]
fn start_send(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<(), String> {
    if paths.is_empty() {
        let msg = "No files selected".to_string();
        emit_error(&app, "other", &msg);
        return Err(msg);
    }
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let (tx, cancel) = begin_task(&app, &state);
    tauri::async_runtime::spawn(async move {
        let cfg = peerly_core::Config::from_env();
        let result = peerly_core::send(&cfg, paths, tx, cancel.clone()).await;
        finish(&app, &cancel, result);
    });
    Ok(())
}

#[tauri::command]
fn start_receive(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
    dest: String,
) -> Result<(), String> {
    if code.trim().is_empty() {
        let msg = "Empty code".to_string();
        emit_error(&app, "invalid_code", &msg);
        return Err(msg);
    }
    let (tx, cancel) = begin_task(&app, &state);
    let (offer_tx, offer_rx) = oneshot::channel::<bool>();
    *state.offer.lock().unwrap() = Some(offer_tx);
    tauri::async_runtime::spawn(async move {
        let cfg = peerly_core::Config::from_env();
        // The core emits `Event::Offer`; we just wait for the user's answer.
        let decide =
            |_manifest: peerly_core::Manifest| async move { offer_rx.await.unwrap_or(false) };
        let result = peerly_core::receive(
            &cfg,
            code.trim(),
            Path::new(&dest),
            decide,
            tx,
            cancel.clone(),
        )
        .await;
        finish(&app, &cancel, result);
    });
    Ok(())
}

#[tauri::command]
fn respond_offer(state: State<'_, AppState>, accept: bool) {
    if let Some(tx) = state.offer.lock().unwrap().take() {
        let _ = tx.send(accept);
    }
}

#[tauri::command]
fn cancel(state: State<'_, AppState>) {
    if let Some(token) = state.task.lock().unwrap().take() {
        token.cancel();
    }
    state.offer.lock().unwrap().take();
}

#[tauri::command]
fn default_download_dir(app: AppHandle) -> String {
    app.path()
        .download_dir()
        .or_else(|_| app.path().home_dir())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Serialize, Default)]
struct PathsInfo {
    files: u64,
    bytes: u64,
}

fn walk(path: &Path, info: &mut PathsInfo, depth: u32) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.is_file() {
        info.files += 1;
        info.bytes += meta.len();
    } else if meta.is_dir()
        && depth < 64
        && let Ok(rd) = std::fs::read_dir(path)
    {
        for entry in rd.flatten() {
            walk(&entry.path(), info, depth + 1);
        }
    }
}

/// File count and total size of the selection (folders are walked recursively).
/// Opens a directory in the system file manager. Only existing directories are
/// accepted, so the webview cannot use this to launch files or programs.
#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("not a directory".into());
    }
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspect_paths(paths: Vec<String>) -> PathsInfo {
    tauri::async_runtime::spawn_blocking(move || {
        let mut info = PathsInfo::default();
        for p in &paths {
            walk(Path::new(p), &mut info, 0);
        }
        info
    })
    .await
    .unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_send,
            start_receive,
            respond_offer,
            cancel,
            default_download_dir,
            open_folder,
            inspect_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Peerly");
}
