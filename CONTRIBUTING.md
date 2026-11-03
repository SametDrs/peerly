# Contributing

Thanks for helping! Read [docs/architecture.md](docs/architecture.md) first, especially before changing anything that crosses crate boundaries. `peerly-proto` is the wire contract: changing it is a protocol change.

## Develop

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Desktop frontend: `cd apps/desktop && npm ci && npm run build`.

Code, comments and docs are in English. By contributing you agree your work is licensed under MIT OR Apache-2.0.
