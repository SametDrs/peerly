# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Peerly: open-source desktop app for direct peer-to-peer file/folder transfer. Sender picks files, gets a code like `42-river-candle`, receiver types it, bytes flow over direct QUIC. Rust workspace + Tauri 2 desktop app (`apps/desktop`, Svelte + TS). The user communicates in Turkish; reply in Turkish, keep code/comments/docs in English.

## Environment (Windows)

`cargo` is not on PATH in fresh PowerShell sessions. Prefix commands with:

```powershell
$env:Path += ";$env:USERPROFILE\.cargo\bin"
```

## Commands

```powershell
cargo check --workspace
cargo test -p peerly-transfer                 # one crate (preferred: other crates may be mid-work)
cargo test -p peerly-proto frame_roundtrip    # single test by name filter
cargo clippy -p <crate> -- -D warnings
cargo run -p peerly-rendezvous -- --listen 0.0.0.0:9009
cargo run -p peerly-cli -- send <paths...>    # binary name: peerly
cargo run -p peerly-cli -- receive <code> --to <dir>
```

Pairing works with no server on the LAN (mDNS + TCP mailbox). `PEERLY_RENDEZVOUS` (or `--rendezvous`) additionally enables a rendezvous server; it is unset by default. `PEERLY_NO_LAN=1` disables LAN mode. Real-mDNS tests are `#[ignore]` (CI lacks multicast): `cargo test -p peerly-pairing -- --ignored`.
Desktop app: `cd apps/desktop; npm install; npm run tauri dev`.

`spike-quic/` is a throwaway Stage 0 speed test, excluded from the workspace (build it from inside its own folder).

## Architecture

Read `docs/architecture.md` before changing anything that crosses crate boundaries. Key points:

- **`peerly-proto` is the wire contract.** Rendezvous messages are JSON over WebSocket; UDP probe, pairing payloads and transfer control frames are postcard (transfer frames are 4-byte BE length-prefixed). Changing a type here is a protocol change: bump `PROTOCOL_VERSION` and update the doc.
- **Dependency direction:** `proto` ← `transfer`, `pairing` ← `core` ← `cli`, desktop. `rendezvous` depends only on `proto`. UIs only talk to `peerly-core`.
- **Separation of concerns:** `peerly-pairing` turns a code into an authenticated `quinn::Connection` (`Paired`); `peerly-transfer` only moves files over a given connection and knows nothing about codes/NAT.
- **Security model:** SPAKE2 (password = full code) over a mailbox (LAN TCP or rendezvous relay; DHT planned) → key that seals each peer's `PeerInfo` (cert SHA-256 fingerprint + candidate addresses). QUIC uses self-signed certs pinned to those fingerprints on both sides. The rendezvous server never sees file data or plaintext `PeerInfo`. Nameplates are single-use, so a wrong guess burns the code.
- **Roles:** sender = QUIC server, receiver = QUIC client. The UDP socket used for the reflexive-address probe is the same one handed to the quinn endpoint (needed for hole punching). Rendezvous UDP probe listens on the same port number as its WebSocket.
- **Transfer:** sender opens one bi control stream (`Offer` → `Accept`/`Reject` → … → `Done`), then one uni stream per file (several in parallel): header frame + raw bytes + 32-byte BLAKE3 trailer. Receiver writes `*.peerly-part`, verifies, renames. Receivers must re-validate every manifest path (no absolute paths, `..`, drive letters, reserved Windows names).
- **Desktop events:** `peerly_core::Event` is serde-tagged (`{"kind": ...}`) and emitted on Tauri event `peerly://event`; commands are `start_send`, `start_receive`, `respond_offer`, `cancel`.

## Team workflow

Work is split among sub-agents with path ownership — see `docs/team.md`. `peerly-proto`, `docs/architecture.md` and the root `Cargo.toml` are coordinator-owned; add new dependencies to the individual crate's `Cargo.toml`, not the workspace root.
