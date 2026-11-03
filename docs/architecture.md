# Peerly Architecture

Peerly sends files and folders directly between two computers. The sender picks
files, gets a short code (`42-river-candle`), the receiver types it, and the
bytes flow peer-to-peer over QUIC. No server ever sees file contents.

## Components

```
            ┌──────────────────────────┐
            │  peerly-rendezvous       │  WebSocket (JSON) + UDP probe, same port
            │  (small public server)   │  pairs peers by nameplate, relays ~4 tiny msgs
            └─────────▲──────▲─────────┘
                      │      │
  ┌───────────────────┴┐    ┌┴───────────────────┐
  │ Sender             │    │ Receiver           │
  │ desktop app / CLI  │    │ desktop app / CLI  │
  │   peerly-core      │    │   peerly-core      │
  │   ├ peerly-pairing │◄══►│   ├ peerly-pairing │  direct QUIC (UDP), mutually pinned certs
  │   └ peerly-transfer│    │   └ peerly-transfer│
  └────────────────────┘    └────────────────────┘
```

| Crate / app | Kind | Responsibility |
|---|---|---|
| `crates/peerly-proto` | lib | **The contract.** All wire types + framing. Every other crate depends on it. |
| `crates/peerly-transfer` | lib | Moves files over a given `quinn::Connection`: manifest, parallel streams, BLAKE3 verify, progress, path safety. |
| `crates/peerly-pairing` | lib | Code generation/parsing, rendezvous client, SPAKE2, address discovery, hole punching, QUIC endpoint with cert pinning. Produces `Paired`. |
| `crates/peerly-core` | lib | Facade: `send(paths)` / `receive(code)`, one serializable `Event` stream. Only API used by UIs. |
| `crates/peerly-rendezvous` | bin | Public server. Nameplate allocation, relay, UDP reflexive address probe. Stateless besides in-memory nameplates. |
| `crates/peerly-cli` | bin `peerly` | `peerly send <paths...>` / `peerly receive <code> [--to dir]`. Testing and power users. |
| `apps/desktop` | Tauri 2 app | Svelte + TS UI. Rust side calls `peerly-core` and forwards `Event`s to the webview. |
| `spike-quic/` | throwaway | Stage 0 speed test. Not part of the workspace. |

Dependency direction: `proto` ← `transfer`, `pairing` ← `core` ← `cli`, `desktop`. `rendezvous` depends only on `proto`.

## Pairing flow

1. **Sender** connects to rendezvous (WebSocket), `Hello`, `Allocate` → nameplate `42`.
   Generates code `42-<word>-<word>` (2 words from a ~1024+ word ASCII list).
2. **Receiver** parses the code, connects, `Hello`, `Claim{42}`. Sender gets `PeerJoined`.
3. Both run **SPAKE2** (symmetric) with password = full code string, exchanging
   `PairingMsg::Pake` through `Relay`. Result: shared key `K`.
   A wrong code fails here without leaking anything; the nameplate is single-use,
   so an attacker gets exactly one guess.
4. Each side binds a UDP socket, sends a `ProbeRequest` to the rendezvous UDP port
   to learn its public (reflexive) address, collects LAN addresses, creates a
   self-signed cert, and sends `PairingMsg::Sealed(PeerInfo{fingerprint, candidates})`
   encrypted with ChaCha20-Poly1305 under `HKDF-SHA256(K, "peerly v1 peerinfo <role>")`.
   Decryption failure = wrong code.
5. The same UDP socket is handed to a `quinn::Endpoint`. **Sender = QUIC server,
   receiver = QUIC client.** Sender sends a few punch datagrams to the receiver's
   candidates; receiver tries all candidates in parallel (LAN first), first
   handshake wins. Both sides pin the peer's cert fingerprint (mutual TLS, no CA).
   ALPN `peerly/1`.
6. Rendezvous connection is closed. File data never touches the server.

**Relay fallback** (symmetric NAT, both sides): not in v1. Pairing reports
`PeerUnreachable`. The design leaves room for a relay later (TURN-like, still E2E
encrypted by QUIC).

## Transfer flow

Over the paired connection (`peerly_proto::transfer`):

1. Sender opens a bidirectional **control stream**, sends `Offer(Manifest)`.
2. Receiver validates every path (relative, no `..`, no absolute, no drive letters,
   no reserved Windows names), checks free disk space, asks the user, replies `Accept`/`Reject`.
3. Sender opens one **unidirectional stream per file**, up to 4–8 concurrently
   (helps folders of many small photos). Each stream: `frame(StreamHeader)`,
   raw bytes, 32-byte BLAKE3 hash computed while streaming.
4. Receiver writes to `<file>.peerly-part`, hashes while writing, verifies, renames.
5. Receiver sends `Done`; sender closes the connection.

QUIC transport is tuned for throughput (large stream/connection windows, see
the spike: 64 MiB stream / 128 MiB connection).

## Events (UI contract)

`peerly_core::Event` is serialized with serde (`{"kind": "...", ...}`) and emitted
to the desktop webview on the Tauri event `peerly://event`. Desktop commands:

| Command | Args | Notes |
|---|---|---|
| `start_send` | `paths: string[]` | Emits `code`, `waiting_for_peer`, `connected`, `started`, `progress`..., `completed` |
| `start_receive` | `code: string, dest: string` | Emits `connecting`, `connected`, `offer` ... |
| `respond_offer` | `accept: boolean` | Answers the pending `offer` |
| `cancel` | – | Cancels the running operation |

Errors: the command's promise rejects with a human-readable message, and an
`error` event `{kind:"error", message}` is emitted by the desktop layer.

## v1 scope (decided)

In: files **and folders**, code pairing, E2E encryption (QUIC/TLS 1.3 + SPAKE2-pinned certs),
LAN + internet direct connection, progress/speed, cancel, accept/reject, integrity check.
Windows first; code stays cross-platform (macOS/Linux builds in CI).

Later: resume, relay fallback, multiple receivers, transfer history, web version.
