# peerly-rendezvous

Pairs two peers by short nameplate, relays small opaque pairing messages, and answers UDP address probes. It never sees file data.

## Run

    cargo run -p peerly-rendezvous -- --listen 0.0.0.0:9009
    RUST_LOG=debug ...   # log level; relay payloads are never logged

Docker (from the workspace root):

    docker build -f crates/peerly-rendezvous/Dockerfile -t peerly-rendezvous .
    docker run -p 9009:9009/tcp -p 9009/udp peerly-rendezvous

## Ports

- TCP 9009: WebSocket (JSON `ClientMsg`/`ServerMsg`).
- UDP 9009 (same number): `ProbeRequest` -> `ProbeResponse` with the observed source address. Must be reachable directly, not through the proxy.

## Deploy

Run behind a TLS reverse proxy (Caddy, nginx) that terminates TLS and forwards WebSocket upgrades to TCP 9009, so clients use `wss://host/`. Open UDP 9009 directly on the host firewall. Note per-IP rate limits use the TCP peer address, so behind a proxy all clients share the proxy's IP; raise `rate_limit_per_min` accordingly or run the proxy on a path that preserves client addresses.

## Limits

64 KiB WS message, 8 KiB relay payload, 32 relays per nameplate, 30 Allocate/Claim per IP per minute, 15 min idle timeout, 100k nameplates, unclaimed nameplates expire after 600 s.
