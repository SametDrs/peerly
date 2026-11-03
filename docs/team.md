# Team & ownership

The project is built by one coordinator and five AI sub-agents. Each agent owns
specific paths and must not edit paths owned by others. Shared contracts
(`crates/peerly-proto`, `docs/architecture.md`, root `Cargo.toml`) are owned by
the coordinator; agents propose changes in their final report instead of editing them.

| Agent | Model | Phase | Owns | Deliverable |
|---|---|---|---|---|
| **transfer** | Opus | 1 | `crates/peerly-transfer/**` | Transfer engine + tests over a local QUIC loopback |
| **pairing** | Opus | 1 | `crates/peerly-pairing/**` | Codes, rendezvous client, SPAKE2, probe, hole punch, pinned QUIC |
| **rendezvous** | Sonnet | 1 | `crates/peerly-rendezvous/**` | Server binary + tests |
| **desktop** | Sonnet | 1 → 2 | `apps/desktop/**` | Tauri 2 + Svelte UI (mock backend in phase 1, real `peerly-core` in phase 2) |
| **integration** | Sonnet | 2 | `crates/peerly-core/**`, `crates/peerly-cli/**`, `tests/`, `.github/`, `README.md`, `LICENSE-*`, `CONTRIBUTING.md` | Facade, CLI, end-to-end test, CI, open-source files |

Rules for every agent:
- Build/test only your crate: `cargo test -p <crate>` (other crates may be mid-work).
- New dependencies go into **your crate's** `Cargo.toml` (`cargo add -p <crate> <dep>`), not the workspace root.
- Keep public signatures in your `lib.rs` as given; if one must change, say so in your report.
- Finish with `cargo clippy -p <crate> -- -D warnings` and `cargo test -p <crate>` passing.
- Do not commit; the coordinator reviews and commits.
- Final report: max ~15 lines — what works, what's tested, open issues, contract change requests.
