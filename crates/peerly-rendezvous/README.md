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

Run behind a TLS reverse proxy (Caddy, nginx) that terminates TLS and forwards WebSocket upgrades to TCP 9009, so clients use `wss://host/`. Open UDP 9009 directly on the host firewall. By default per-IP limits (rate limit and connection cap) use the TCP peer address, so behind a proxy all clients would share the proxy's IP. Start the server with `--trust-proxy` to use the right-most `X-Forwarded-For` entry instead (the one your proxy appends; the header is ignored without the flag). Only enable it when the server is reachable exclusively through such a proxy, otherwise clients can forge the header.

Users behind one NAT (CGNAT, offices, universities) share the same IP and therefore share the per-IP limits. If that causes false positives, raise `--max-connections-per-ip` / `rate_limit_per_min`.

## Limits

64 KiB WS message, 8 KiB relay payload, 32 relays per nameplate, 30 Allocate/Claim per IP per minute, 15 min idle timeout, 10 s to complete the WebSocket handshake and send `Hello` (`--hello-timeout-secs`), 10,000 concurrent connections (`--max-connections`), 32 concurrent connections per IP (`--max-connections-per-ip`), 100k nameplates, unclaimed nameplates expire after 600 s.

UDP probe: only exactly-sized requests are answered, the reply is at most 15 bytes larger than the request (under 2x amplification), and each source IP is limited to 60 probes per minute.
