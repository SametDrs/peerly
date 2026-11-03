# Peerly

Send files and folders directly from one computer to another. No account, no upload to a server, no size limit imposed by a cloud service.

> Status: **pre-alpha**. Expect rough edges and protocol changes.

## Why

- **Direct**: bytes flow peer to peer over QUIC. A small rendezvous server only helps two devices find each other and never sees your files.
- **Fast**: transfers use several parallel QUIC streams and go over the local network when both devices share one.
- **Encrypted**: every connection is encrypted and authenticated; files are verified with BLAKE3 hashes.

## How it works

1. The sender picks files and gets a short code such as `42-river-candle`.
2. The receiver types the code. Both sides contact the rendezvous server using the code's number (a single-use "nameplate").
3. They run a SPAKE2 key exchange (password = the full code) over the relay, then exchange sealed connection info: certificate fingerprint and candidate addresses.
4. A direct QUIC connection is established (LAN address or hole-punched), with each side pinning the other's self-signed certificate.
5. Files are streamed over that connection.

See [docs/architecture.md](docs/architecture.md) for details.

## Build and run

Requires a recent stable Rust toolchain.

```sh
# rendezvous server
cargo run -p peerly-rendezvous -- --listen 0.0.0.0:9009

# CLI (set PEERLY_RENDEZVOUS or pass --rendezvous ws://host:9009)
cargo run -p peerly-cli -- send ./some-folder other-file.zip
cargo run -p peerly-cli -- receive 42-river-candle --to ./downloads

# desktop app (Tauri 2 + Svelte)
cd apps/desktop
npm install
npm run tauri dev
```

## Security model

- The code is the shared secret. SPAKE2 turns it into a key without sending it; a wrong guess fails and, because nameplates are single-use, burns the code.
- That key seals each peer's certificate fingerprint and addresses, so the rendezvous server cannot read or swap them.
- QUIC uses self-signed certificates pinned to those fingerprints.
- The receiver validates every incoming path (no absolute paths, `..`, reserved names) and never overwrites existing files.

This project has not been independently audited.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

## Türkçe özet

Peerly, dosya ve klasörleri bir bilgisayardan diğerine doğrudan (sunucuya yüklemeden) göndermeyi sağlayan açık kaynaklı bir uygulamadır. Gönderen `42-river-candle` gibi kısa bir kod alır, alıcı bu kodu girer; cihazlar kodla eşleşip SPAKE2 ile güvenli bir anahtar üretir ve doğrudan şifreli QUIC bağlantısı üzerinden dosyaları aktarır. Aracı sunucu dosyalarınızı hiçbir zaman görmez. Proje henüz **pre-alpha** aşamasındadır. Lisans: MIT veya Apache-2.0.
