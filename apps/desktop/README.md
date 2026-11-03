# Peerly desktop

Tauri 2 + Svelte 5 + TypeScript + Vite. The Rust side (`src-tauri`) calls `peerly-core` and forwards its events to the webview.

```powershell
$env:Path += ";$env:USERPROFILE\.cargo\bin"
cd apps/desktop
npm install
npm run tauri dev      # dev app with hot reload
npm run build          # frontend only (dist/)
npm run check          # svelte-check
npm run tauri build    # installer / bundle
cargo clippy -p peerly-desktop -- -D warnings
```

Commands: `pick_files`, `pick_folder`, `pick_dest_dir`, `clear_selection`, `get_selection`, `inspect_paths`, `set_drop_enabled`, `start_send`, `start_receive`, `respond_offer`, `cancel`, `open_folder`.
The webview never supplies file paths: Rust owns the send selection and receive destination (see `docs/architecture.md`).
Events: `peerly://event` carrying `peerly_core::Event` (tag `kind`) plus `{kind:"error", code, message}`, and `peerly://selection` with the current selection.
UI strings live in `src/lib/i18n.ts` (English and Turkish, chosen from `navigator.language`).
