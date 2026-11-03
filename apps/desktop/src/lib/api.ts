import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Mirrors `peerly_core::Event` (serde tag "kind") plus the desktop layer's `error`. */
export type PeerlyEvent =
  | { kind: "code"; code: string }
  | { kind: "waiting_for_peer" }
  | { kind: "connecting" }
  | { kind: "connected"; route: string }
  | { kind: "offer"; name: string; files: number; total_bytes: number }
  | { kind: "started"; total_bytes: number; total_files: number }
  | { kind: "progress"; bytes_done: number; bytes_total: number; bytes_per_sec: number }
  | { kind: "file_completed"; path: string }
  | { kind: "completed"; files: number; bytes: number; elapsed_ms: number }
  | { kind: "error"; code: string; message: string };

export const onPeerlyEvent = (cb: (e: PeerlyEvent) => void): Promise<UnlistenFn> =>
  listen<PeerlyEvent>("peerly://event", (e) => cb(e.payload));

/**
 * The backend owns every user-chosen path (send selection + receive folder).
 * They come only from native dialogs opened by Rust or from OS drag & drop;
 * the webview just displays them.
 */
export type Selection = { paths: string[]; dest: string };

export const onSelection = (cb: (s: Selection) => void): Promise<UnlistenFn> =>
  listen<Selection>("peerly://selection", (e) => cb(e.payload));

export const getSelection = () => invoke<Selection>("get_selection");
export const pickFiles = () => invoke<Selection>("pick_files");
export const pickFolder = () => invoke<Selection>("pick_folder");
export const pickDestDir = () => invoke<Selection>("pick_dest_dir");
export const clearSelection = () => invoke<Selection>("clear_selection");
/** Whether OS drops are added to the selection (only on the send/pick screen). */
export const setDropEnabled = (enabled: boolean) => invoke<void>("set_drop_enabled", { enabled });

/** Sends the current selection. */
export const startSend = () => invoke<void>("start_send");
/** Receives into the current destination folder. */
export const startReceive = (code: string) => invoke<void>("start_receive", { code });
export const respondOffer = (accept: boolean) => invoke<void>("respond_offer", { accept });
export const cancel = () => invoke<void>("cancel");
/** Opens the current destination folder. */
export const openFolder = () => invoke<void>("open_folder");
/** File count and total size of the current selection. */
export const inspectSelection = () => invoke<{ files: number; bytes: number }>("inspect_paths");
