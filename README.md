# kino

A Tauri 2 multiplatform streaming client consuming the Stremio addon ecosystem,
with an embedded torrent engine, adaptive piece-deadline buffering, and a
10-foot UI tuned for Android TV, mobile Android, and desktop Linux.

The authoritative product specification lives in [`PRD.md`](./PRD.md). It is
locked at version 1.0 and immutable except by human edit. Agent state and
session history live in [`STATE.md`](./STATE.md).

## Targets (v1)

- Android phone (arm64, touch-primary)
- Android TV (Shield Pro 2019 reference, D-pad + gamepad primary)
- Linux x86_64 desktop (keyboard + mouse primary)

Distribution is sideload-only in v1. No app stores.

## Stack

| Layer | Choice |
|---|---|
| Host | Tauri 2 |
| Backend | Rust (workspace of 5 crates) |
| Frontend | SolidJS + Vite + TailwindCSS |
| Persistence | SQLite via `sqlx` (WAL) |
| Torrent engine | `librqbit` |
| Local stream server | `axum` on `127.0.0.1` |
| Player (Android) | ExoPlayer / Media3 in a native Kotlin activity |
| Player (Linux) | libmpv via `libmpv-rs` |

See PRD §3 for the full stack table and architectural lock-ins.

## Workspace layout

```
crates/
  kino-core/        Shared types, settings, install_id, db
  kino-torrent/     librqbit wrapper + adaptive buffer scheduler
  kino-server/      axum HTTP server for player consumption
  kino-addons/      Stremio addon protocol client
  kino-metadata/    TMDB / Trakt / TVDB / Fanart.tv clients
src-tauri/          Tauri host binary
frontend/           SolidJS app (single bundle for all three targets)
android/
  keystore/         Stable sideload keystore (see below)
  player-plugin/    Native Kotlin ExoPlayer plugin
migrations/         sqlx migrations
.github/workflows/  CI + release
```

## Building

Prerequisites: a stable Rust toolchain (the project pins one via
`rust-toolchain.toml`), Node 20+, Java 17+ (for Android), Android SDK + NDK
(for Android targets), and `cargo-tauri` (`cargo install tauri-cli --version
"^2.0.0"`).

```sh
# Rust workspace (no Tauri, no frontend)
cargo build --workspace
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace

# Full app (once src-tauri and frontend are wired up)
cargo tauri build                          # Linux desktop
cargo tauri android build                  # Android universal APK
```

Build artifacts produced by CI for tagged releases are listed in PRD §F-018.

## Android keystore

`android/keystore/kino-dev.keystore` is committed by design for sideload
reproducibility. Anyone with the repo can sign APKs as kino, which is the
trade-off that lets sideloaded updates reinstall over previous versions on
real devices without a fresh install. **It is not a security control.** For
app-store distribution (out of v1 scope), a private keystore stored as GitHub
secrets would be generated. See [`android/keystore/README.md`](./android/keystore/README.md)
for the keystore parameters.

## Contributing

`PRD.md` is the source of truth. Read it before opening a PR. Any change that
contradicts the PRD is rejected by default; PRD revisions are the human's
prerogative.

Development conventions, cross-session decisions, and the running feature
tracker live in [`STATE.md`](./STATE.md).

## License

MIT. See [`LICENSE`](./LICENSE).
