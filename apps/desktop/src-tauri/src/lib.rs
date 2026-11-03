use std::path::{Path, PathBuf};
use std::sync::Mutex;

use peerly_core::Event;
use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const EVENT_NAME: &str = "peerly://event";
/// Emitted with a [`SelectionView`] whenever the selection or destination changes.
const SELECTION_EVENT: &str = "peerly://selection";

/// User-chosen paths. The backend owns them: they only ever come from native
/// dialogs opened here or from OS drag & drop events, never from the webview,
/// so a compromised webview cannot point a send (or a receive) at arbitrary paths.
#[derive(Default)]
struct Selection {
    /// What `start_send` sends (files and folders, deduplicated, in pick order).
    paths: Vec<PathBuf>,
    /// Where `start_receive` saves; defaults to the download dir.
    dest: Option<PathBuf>,
    /// Whether OS drops are currently added (the UI turns this on only on the send/pick screen).
    accept_drops: bool,
}

/// What the UI displays; read-only from its side.
#[derive(Serialize, Clone)]
struct SelectionView {
    paths: Vec<String>,
    dest: String,
}

impl Selection {
    fn view(&self) -> SelectionView {
        SelectionView {
            paths: self
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            dest: self
                .dest
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// Shared state: the running task's cancel token, the pending receiver-side
/// offer answer and the user's selection.
#[derive(Default)]
struct AppState {
    task: Mutex<Option<CancellationToken>>,
    offer: Mutex<Option<oneshot::Sender<bool>>>,
    selection: Mutex<Selection>,
}

/// Applies `f` to the selection, broadcasts the result and returns it.
fn update_selection(app: &AppHandle, f: impl FnOnce(&mut Selection)) -> SelectionView {
    let state = app.state::<AppState>();
    let view = {
        let mut sel = state.selection.lock().unwrap();
        f(&mut sel);
        sel.view()
    };
    let _ = app.emit(SELECTION_EVENT, &view);
    view
}

fn add_paths(app: &AppHandle, new: Vec<PathBuf>) -> SelectionView {
    update_selection(app, |sel| {
        for p in new {
            if !sel.paths.contains(&p) {
                sel.paths.push(p);
            }
        }
    })
}

/// A native file dialog parented to the app window (modal on Windows).
fn file_dialog(window: &WebviewWindow) -> FileDialogBuilder<tauri::Wry> {
    window.dialog().file().set_parent(window)
}

fn into_paths(picked: Option<Vec<FilePath>>) -> Vec<PathBuf> {
    picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect()
}

/// OS drag & drop onto the window: the only other source of selection paths.
fn on_drag_drop(app: &AppHandle, event: &DragDropEvent) {
    if let DragDropEvent::Drop { paths, .. } = event {
        let accept = app
            .state::<AppState>()
            .selection
            .lock()
            .unwrap()
            .accept_drops;
        if accept && !paths.is_empty() {
            add_paths(app, paths.clone());
        }
    }
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

/// Sends the current selection (a snapshot: later selection changes do not affect it).
#[tauri::command]
fn start_send(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let paths = state.selection.lock().unwrap().paths.clone();
    if paths.is_empty() {
        let msg = "No files selected".to_string();
        emit_error(&app, "other", &msg);
        return Err(msg);
    }
    let (tx, cancel) = begin_task(&app, &state);
    tauri::async_runtime::spawn(async move {
        let cfg = peerly_core::Config::from_env();
        let result = peerly_core::send(&cfg, paths, tx, cancel.clone()).await;
        finish(&app, &cancel, result);
    });
    Ok(())
}

/// Receives into the stored destination directory.
#[tauri::command]
fn start_receive(app: AppHandle, state: State<'_, AppState>, code: String) -> Result<(), String> {
    if code.trim().is_empty() {
        let msg = "Empty code".to_string();
        emit_error(&app, "invalid_code", &msg);
        return Err(msg);
    }
    let Some(dest) = state.selection.lock().unwrap().dest.clone() else {
        let msg = "No destination folder".to_string();
        emit_error(&app, "other", &msg);
        return Err(msg);
    };
    let (tx, cancel) = begin_task(&app, &state);
    let (offer_tx, offer_rx) = oneshot::channel::<bool>();
    *state.offer.lock().unwrap() = Some(offer_tx);
    tauri::async_runtime::spawn(async move {
        let cfg = peerly_core::Config::from_env();
        // The core emits `Event::Offer`; we just wait for the user's answer.
        let decide =
            |_manifest: peerly_core::Manifest| async move { offer_rx.await.unwrap_or(false) };
        let result =
            peerly_core::receive(&cfg, code.trim(), &dest, decide, tx, cancel.clone()).await;
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
fn get_selection(state: State<'_, AppState>) -> SelectionView {
    state.selection.lock().unwrap().view()
}

/// Native multi-file picker; appends to the selection.
#[tauri::command]
async fn pick_files(app: AppHandle, window: WebviewWindow) -> SelectionView {
    let (tx, rx) = oneshot::channel();
    file_dialog(&window).pick_files(move |r| {
        let _ = tx.send(r);
    });
    add_paths(&app, into_paths(rx.await.ok().flatten()))
}

/// Native folder picker; appends to the selection.
#[tauri::command]
async fn pick_folder(app: AppHandle, window: WebviewWindow) -> SelectionView {
    let (tx, rx) = oneshot::channel();
    file_dialog(&window).pick_folder(move |r| {
        let _ = tx.send(r.map(|p| vec![p]));
    });
    add_paths(&app, into_paths(rx.await.ok().flatten()))
}

/// Native folder picker for the receive destination.
#[tauri::command]
async fn pick_dest_dir(app: AppHandle, window: WebviewWindow) -> SelectionView {
    let current = app
        .state::<AppState>()
        .selection
        .lock()
        .unwrap()
        .dest
        .clone();
    let mut dialog = file_dialog(&window);
    if let Some(dir) = current.filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    let (tx, rx) = oneshot::channel();
    dialog.pick_folder(move |r| {
        let _ = tx.send(r.map(|p| vec![p]));
    });
    let picked = into_paths(rx.await.ok().flatten()).pop();
    update_selection(&app, |sel| {
        if let Some(dir) = picked {
            sel.dest = Some(dir);
        }
    })
}

#[tauri::command]
fn clear_selection(app: AppHandle) -> SelectionView {
    update_selection(&app, |sel| sel.paths.clear())
}

/// Turns adding OS drag & drop paths to the selection on or off. Turning it on
/// grants nothing by itself: paths still have to be dropped by the user.
#[tauri::command]
fn set_drop_enabled(state: State<'_, AppState>, enabled: bool) {
    state.selection.lock().unwrap().accept_drops = enabled;
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

/// Opens the stored receive destination in the system file manager, and only
/// if it is an existing directory, so this cannot launch files or programs.
#[tauri::command]
fn open_folder(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = state.selection.lock().unwrap().dest.clone();
    let Some(dir) = dir.filter(|d| d.is_dir()) else {
        return Err("not a directory".into());
    };
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// File count and total size of the current selection (folders are walked recursively).
#[tauri::command]
async fn inspect_paths(app: AppHandle) -> PathsInfo {
    let paths = app
        .state::<AppState>()
        .selection
        .lock()
        .unwrap()
        .paths
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut info = PathsInfo::default();
        for p in &paths {
            walk(p, &mut info, 0);
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
        .setup(|app| {
            let dest = app
                .path()
                .download_dir()
                .or_else(|_| app.path().home_dir())
                .ok();
            app.state::<AppState>().selection.lock().unwrap().dest = dest;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::DragDrop(ev) = event {
                on_drag_drop(window.app_handle(), ev);
            }
        })
        .on_webview_event(|webview, event| {
            if let tauri::WebviewEvent::DragDrop(ev) = event {
                on_drag_drop(webview.app_handle(), ev);
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_send,
            start_receive,
            respond_offer,
            cancel,
            get_selection,
            pick_files,
            pick_folder,
            pick_dest_dir,
            clear_selection,
            set_drop_enabled,
            open_folder,
            inspect_paths
        ])
        .run(tauri::generate_context!())
        .expect("error while running Peerly");
}
