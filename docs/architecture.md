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
6. The receiver may complete several handshakes in parallel; it opens a uni
   stream on the one it picked and writes a single byte `0x01`. The sender keeps
   that connection and drops the rest. `peerly-transfer` never sees this stream.
7. Rendezvous connection is closed. File data never touches the server.

Pairing timeouts: rendezvous connect 10 s, each relay exchange step 30 s, direct
connect 15 s (`PeerUnreachable`). Waiting for a receiver has no timeout besides
the server's nameplate TTL. Typos in the words give `InvalidCode` locally (both
words must be in the list), so they don't burn the nameplate. IPv4 only in v1.

### Mailboxes (how the pairing messages travel)

Steps 3–4 only need a channel for a handful of opaque messages, called a
*mailbox*. The rendezvous server is one mailbox and is **optional**:

| Mailbox | When | Notes |
|---|---|---|
| LAN (mDNS + TCP) | always on unless `PEERLY_NO_LAN=1` | No server. See below. |
| Rendezvous (WebSocket) | only if `PEERLY_RENDEZVOUS` / `--rendezvous` is set | As described above. |
| DHT | always on unless `PEERLY_NO_DHT=1` | Internet pairing without our own server (BitTorrent Mainline DHT + public STUN). See below. |

**LAN mode** (`peerly_proto::lan`): the sender listens on TCP and advertises the
mDNS service `_peerly._tcp.local.` with TXT `np=<nameplate>`, `v=<version>`.
Without a rendezvous server, the sender generates the nameplate itself (1000..=9999).
The receiver browses for about 2 s, connects to each match, sends
`frame(LanHello)`, and then the same `PairingMsg` sequence runs over
length-prefixed frames. Several senders on one LAN can share a nameplate, so the
sender tolerates up to 3 failed SPAKE attempts on the LAN mailbox before giving
up. That is still about 3 guesses in a million. The sender races all of its
mailboxes and the first completed pairing wins. The listener and the mDNS
advertisement are torn down as soon as pairing ends, fails or is cancelled.
The LAN path skips the UDP probe; candidates are LAN addresses only.

**DHT mode** (`peerly_proto::dht`): the mailbox is four BEP 44 mutable items
on the public BitTorrent Mainline DHT, one per message:
- `s1`: sender's PAKE message;
- `r1`: receiver's PAKE message;
- `s2`, `r2`: each side's sealed `PeerInfo`.

The items are signed with a key derived from the nameplate alone. That key is
an address, not a secret; the code words never touch the DHT, so observers can't
test guesses offline. Each item carries the sender's random session id and a
timestamp, so stale items are ignored. Before showing a code, the sender checks
`s1`; if it is taken, the sender picks another nameplate. The reflexive address
comes from public STUN servers, queried on the same UDP socket that becomes the
QUIC endpoint. Pairing over the DHT takes seconds, not milliseconds: about 15 s
end to end in testing, and the code itself is ready in about 3 s.

The two sides learn each other's candidates at different moments. To cover
that, a sender on the DHT path accepts and punches for 45 s: one 30 s poll
step plus the receiver's 15 s connect window. A receiver that cannot find
`s1` within 30 s reports `CodeNotFound`. The DHT client binds a random UDP
port, not 6881. Nameplate keys are public, so anyone can overwrite a slot.
That makes DoS possible, but it gives no offline guessing material.

Locally generated nameplates are 4 digits (1000..=9999). Senders race every
enabled mailbox. Receivers run LAN and DHT lookups concurrently, and a LAN match
is preferred.

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

Contract details:
- `FileEntry.index` equals the entry's position in `Manifest.files`; anything else is a protocol error.
- QUIC application close codes: `0` done (also used after `Reject`), `1` cancel, `2` error. The close reason carries a short human-readable message, reported by the peer as `PeerCancelled(reason)`.
- The control stream has higher priority than data streams. On abort the sender resets its data streams so they don't hold up the `Cancel` frame; the receiver waits up to 1 s after a stream reset for that `Cancel` to arrive.
- A receiver that declines gets `TransferError::Rejected` from `peerly_transfer::receive`; `peerly-core` treats that as a normal outcome on the receiver side.
- Senders validate their own manifest with the same path rules and fail early with `UnsafePath`. Duplicate paths are compared case-insensitively on Windows and macOS.
- Existing files are never overwritten: a colliding top-level item becomes `name (1).ext` / `folder (1)`. Files are not fsynced before rename (small-file speed).

QUIC transport is tuned for throughput (large stream/connection windows:
64 MiB stream / 128 MiB connection).

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
