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

export const startSend = (paths: string[]) => invoke<void>("start_send", { paths });
export const startReceive = (code: string, dest: string) => invoke<void>("start_receive", { code, dest });
export const respondOffer = (accept: boolean) => invoke<void>("respond_offer", { accept });
export const cancel = () => invoke<void>("cancel");
export const defaultDownloadDir = () => invoke<string>("default_download_dir");
export const inspectPaths = (paths: string[]) =>
  invoke<{ files: number; bytes: number }>("inspect_paths", { paths });
