# Peerly desktop

Tauri 2 + Svelte 5 + TypeScript + Vite. Talks to `peerly-core` (currently via a mock backend, see `src-tauri/src/mock.rs` and `USE_MOCK` in `src-tauri/src/lib.rs`).

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

Commands: `start_send`, `start_receive`, `respond_offer`, `cancel`, `default_download_dir`, `inspect_paths`.
Events: `peerly://event` carrying `peerly_core::Event` (tag `kind`) plus `{kind:"error", message}`.
UI strings live in `src/lib/i18n.ts` (English and Turkish, chosen from `navigator.language`).
