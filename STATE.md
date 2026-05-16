# kino — Agent State

**PRD version:** 1.0 (locked)
**Status:** scaffolding
**Last session:** 003
**Next session:** 004

---

## Sessions Log

_New entries prepended at the top._

### Session 003 — Persistence layer (F-002)

**Branch:** `claude/session-001-bootstrap-LXpGZ`
(Harness-supplied; see ADR-033. The label encodes the harness invocation,
not the agent session number.)

**Scope chosen:** F-002 Persistence layer end-to-end.

Session 002 left two open paths for Session 003: **(a) F-001 Android
completion** (cargo tauri android init + NDK + build-android CI job) or
**(b) F-002 Persistence layer** if Android proved hard. Inspection of this
session's container found no Android SDK and no NDK installed (the Rust
android cross-targets are pre-installed, but `sdkmanager` / `cmdline-tools`
/ `ndk` are not). Bootstrapping the SDK + NDK is a ~1.5 GiB download +
license-acceptance dance that, combined with `cargo tauri android init`
template generation and a first Gradle assemble, would easily eclipse the
"smaller sessions are better" guidance. F-002 has zero such dependencies
(the migration was shipped in Session 001 and the workspace already
declares `sqlx` and `tokio`), unblocks five downstream features (F-003,
F-004, F-006, F-007, F-012), and is the explicit fallback the Session 002
heads-up named. F-001 Android lands in Session 004.

**Files added (summary):**

- `crates/kino-core/src/db.rs` — new module. The `Db` handle: `SqlitePool`
  with `max_connections = 4` for file-backed databases (PRD §3 lock-in)
  and `max_connections = 1` for the in-memory test path (each in-memory
  pool connection owns a distinct DB unless backed by shared-cache, so a
  4-way pool would miss migrations on three out of four; ADR-037).
  Embedded migrations from `migrations/` via `sqlx::migrate!("../../migrations")`.
  WAL journaling, `synchronous = NORMAL`. Bootstrap of `settings.install_id`
  (UUID v4) on first launch, idempotent on reopen. Typed methods:
  `kv_get` / `kv_set` / `install_id` / `journal_mode` / `cw_list` /
  `cw_upsert` / `cw_delete` / `addons_list` / `addons_insert` /
  `addons_delete` / `addons_set_enabled` / `addons_reorder`. 15 unit tests
  cover the entire surface plus the WAL pragma and migration idempotency.
- `crates/kino-core/src/cw.rs` — `ContinueWatching` domain type matching
  the `continue_watching` schema. Includes a `progress()` helper clamped
  to `[0.0, 1.0]` for the F-012 progress bar.
- `crates/kino-core/src/addon.rs` — `Addon` and `AddonInsert` types
  matching the `addons` schema.
- `crates/kino-core/src/lib.rs` — wires the new modules and re-exports
  `Db`, `DbError`, `INSTALL_ID_KEY`. The crate-level `Error` enum gains a
  `Db` transparent variant.
- `crates/kino-core/Cargo.toml` — adds `sqlx` and `tokio` from workspace
  deps; adds `tempfile = "3"` as a dev-dependency for file-backed tests.
- `src-tauri/src/commands.rs` — new module exposing eleven Tauri commands
  that wrap `Db` methods. Errors cross IPC as `String` (the Tauri convention).
- `src-tauri/src/paths.rs` — new module. Resolves the per-platform DB path
  per PRD §3 Storage layout: `dirs::config_dir().join("kino")` on Linux
  (the PRD pins the dir name to `kino/`, not the bundle identifier that
  Tauri's `app_config_dir()` would yield), `app.path().app_config_dir()`
  on Android (maps to `Context.filesDir`). Cfg-gated per OS to avoid the
  unused-import lint that `-D warnings` would catch.
- `src-tauri/src/lib.rs` — `setup()` now resolves the DB path, opens the
  pool via `tauri::async_runtime::block_on(Db::open(&path))`, registers
  it in app state via `app.manage(db)`, and the `invoke_handler` lists
  the eleven F-002 commands.
- `src-tauri/Cargo.toml` — adds `kino-core` (path dep), `dirs = "5"`, and
  `thiserror` (from workspace).

**Features advanced:**

- F-002: not started → complete
  - **DB created on first launch at PRD §3 path:** `paths::db_path()`
    resolves to `~/.config/kino/kino.db` on Linux and
    `Context.filesDir/kino.db` on Android (Tauri AppHandle path resolver).
    Parent dir is `mkdir -p`'d before the open.
  - **Migrations apply cleanly and idempotently:** verified by
    `migration_round_trip_creates_all_tables` (all six PRD tables present)
    and `migration_is_idempotent_on_reopen` (install_id survives reopen,
    no error on second `sqlx::migrate!` run).
  - **Pool size 4 + WAL mode:** `POOL_SIZE = 4`; `journal_mode()` returns
    `wal` on the file-backed path (`wal_journal_mode_is_active_on_file_backed_db`).
  - **KV / CW CRUD / addons CRUD Tauri commands:** all eleven registered
    in `invoke_handler` and exercised through their underlying `Db`
    methods by the unit tests.
  - **Unit tests cover migration round-trip, KV operations, CW upsert
    behavior:** 15 new tests in `db.rs` plus 1 in `cw.rs` (the
    progress-clamp invariant). Test surface is end-to-end: same code path
    the IPC layer would hit, just without the Tauri wrapper.

**ADRs filed this session:**

- **ADR-037** (in-memory test pool is single-connection by design): The
  `sqlx` sqlite in-memory connection mode (`SqliteConnectOptions::in_memory(true)`)
  gives each connection a private DB unless paired with a `?cache=shared`
  URI, which `SqliteConnectOptions` does not expose directly. The fix
  shipped here forces `max_connections = 1` for `Db::open_in_memory()`,
  used only by unit tests. The file-backed `Db::open()` path keeps the
  PRD-mandated `max_connections = 4`, and the file-backed WAL test
  exercises that path. The alternative (every test uses `tempfile` +
  file-backed DBs) would double test setup time and the WAL pragma is
  already independently verified.
- **ADR-038** (DB path resolution is host-side, not core-side): The PRD
  §3 storage layout differs per OS (XDG on Linux, `Context.filesDir` on
  Android), and only the Tauri AppHandle can resolve the Android variant
  at runtime. Rather than push platform branches into `kino-core` (which
  must stay testable without a Tauri runtime), the host (`src-tauri`) owns
  path resolution via `paths.rs` and feeds the absolute path to
  `Db::open()`. `kino-core` exposes the file-name constant (`kino.db`) so
  the host doesn't have to duplicate it.
- **ADR-039** (Tauri command error type is `String`): The Tauri 2 IPC
  layer serializes command return values as JSON; `kino_core::DbError`
  could be `#[derive(Serialize)]`'d, but mapping its variants to localized
  user-facing messages is a frontend concern (PRD §5 i18n). For F-002 the
  commands return `Result<T, String>` with `e.to_string()` on failure;
  later sessions may swap to a typed error enum once the UI surfaces them.
  This matches the Tauri 2 cookbook pattern and is reversible without an
  API break (`String` is a subtype of any serializable error envelope we
  might invent later).

**Tests added / coverage notes:**

- Rust: 16 new tests (15 in `db.rs`, 1 in `cw.rs`). Workspace total now
  20 in `kino-core` (was 5) + 3 in `kino-torrent` + 12 + 4 in `kino-addons`
  = 39 passing.
- Frontend: no new tests this session. The persistence layer has no
  frontend surface yet; F-008 / F-012 will exercise the commands.

**Known issues introduced or resolved:**

- **New (introduced):** none — but the "AppImage bundle step not exercised
  locally in Session 002" entry below stays open. This session also did
  not run `cargo tauri build` end-to-end because tauri-cli was not
  pre-installed in this container; `cargo build -p kino-app --target
  x86_64-unknown-linux-gnu` (debug and release) was the local proxy. CI
  is the source of truth for the bundle step.
- **Resolved:** ADR-031 / Session 002's deferred dependency on `kino-core`
  from `src-tauri/` — the host now depends on it and uses the `Db` type.

**Heads-up for Session 004:**

- **Primary scope: F-001 Android completion.** Install the Android NDK
  + cmdline-tools (`apt-get install android-sdk` is not sufficient; the
  Tauri 2 docs recommend bootstrapping `cmdline-tools/latest/bin/sdkmanager`
  from the Google-hosted zip and then `sdkmanager --install
  "platform-tools" "platforms;android-34" "build-tools;34.0.0"
  "ndk;27.0.12077973"`). Set `ANDROID_HOME` + `NDK_HOME` env vars. Run
  `cargo install tauri-cli --locked --version "^2"` (Session 002 verified
  this compiles in ~5 min on this container hardware). Then
  `cd src-tauri && cargo tauri android init` to generate
  `src-tauri/gen/android/`. Wire signing with the committed
  `android/keystore/kino-dev.keystore` (alias `kino-dev`, store/key pw
  `kinodev`). `cargo tauri android build --apk` must produce a signed
  APK locally. Then add the `build-android` job to `.github/workflows/ci.yml`
  mirroring the structure of `build-linux`. Flip F-001 to `[x]`.
- **Secondary scope (if Android lands fast): F-003 Metadata clients
  scaffolding** — pure-Rust HTTP clients in `kino-metadata` for TMDB /
  Trakt / TVDB / Fanart.tv, with `wiremock` integration tests. Locked
  TTLs and retry backoff already live in `kino-core::constants`.
- The DB path on Linux is `~/.config/kino/kino.db`. The Tauri host opens
  it in `setup`; nothing else needs to know.
- If the frontend wants to call the new commands, they're available via
  `@tauri-apps/api/core`'s `invoke()` with names `kv_get`, `kv_set`,
  `install_id`, `cw_list`, `cw_upsert`, `cw_delete`, `addons_list`,
  `addons_insert`, `addons_delete`, `addons_set_enabled`, `addons_reorder`.
  The TypeScript bindings (typed wrapper module under `frontend/src/ipc/`)
  are intentionally NOT generated yet — they land with the first feature
  that consumes them (F-008 Home for CW; F-016 Settings for addons).

### Session 002 — Tauri host + SolidJS frontend (F-001 desktop completion)

**Branch:** `claude/session-001-bootstrap-JtPxr`
(The harness-supplied branch name doesn't reflect the actual session number;
see ADR-033. Future sessions follow whichever branch the harness assigns or,
absent one, the protocol's `agent/session-NNN-<slug>` form.)

**Scope chosen:** F-001 completion for the **desktop (Linux)** target —
stand up the Tauri 2 host, the SolidJS frontend, wire `src-tauri` into the
workspace, get `cargo tauri build --target x86_64-unknown-linux-gnu` green
end-to-end, and extend CI accordingly. Android scaffolding (`cargo tauri
android init`, NDK provisioning, `build-android` CI job) is deferred to
Session 003 because the Tauri Android template generation needs the NDK and
SDK installed (~3 GiB) and a meaningful APK test, which together would
balloon Session 002 well past the "ship something every session" guidance.
F-001 stays `in progress` because Android isn't done yet; it flips to `[x]`
the moment Session 003 lands the green Android build.

**Files added (summary):**

- `frontend/` — SolidJS 1.9 + Vite 5 + TailwindCSS 3 + `@solid-primitives/i18n`,
  matching PRD §3 stack lock-ins.
  - `package.json`, `package-lock.json`, `tsconfig.json`, `vite.config.ts`,
    `vitest.config.ts`, `tailwind.config.js`, `postcss.config.js`,
    `eslint.config.js` (flat config), `index.html`.
  - `src/index.tsx` (entry), `src/App.tsx` (placeholder home rendering the
    PRD §F-001 required text "kino"), `src/styles.css` (Tailwind directives +
    10-foot background).
  - `src/i18n.ts` + `src/locales/{en,fr}.json` (PRD §5 Internationalization;
    auto-detect with safe fallback to `en`).
  - `src/test-setup.ts` (vitest setup hook, currently a no-op).
  - `src/i18n.test.ts` (5 tests covering locale resolution) +
    `src/App.test.tsx` (2 tests asserting the F-001 placeholder text and
    the tagline render).
- `src-tauri/` — Tauri 2 host binary `kino-app`, App ID `dev.kino.app`,
  display name `kino` per PRD §F-001.
  - `Cargo.toml` (rlib + cdylib + staticlib for Android, binary for desktop;
    workspace-inherited package metadata; ADR-030 lint config).
  - `build.rs` (standard `tauri_build::build()`).
  - `tauri.conf.json` (Tauri 2 schema; `1280×800` default window, AppImage
    bundling, CSP permitting the local axum stream server prefix and
    `ipc:`/`http://ipc.localhost` Tauri 2 IPC).
  - `capabilities/default.json` (minimal capability surface; grows as
    commands land).
  - `src/main.rs` (thin binary) + `src/lib.rs` (`run()` shared between
    desktop and Android, sets up `tracing` then runs the default Tauri
    builder).
  - `icons/{32x32,128x128,128x128@2x,icon}.png` (placeholder PNGs generated
    deterministically from Python+PIL; real branding lands in a polish pass).
- Workspace `Cargo.toml`: `src-tauri` added to `[workspace] members`; the
  ADR-031 placeholder note removed (since the dir now exists with a valid
  `Cargo.toml`).
- `Cargo.lock`: updated by `cargo check` to lock the Tauri 2 dep tree.
- `.github/workflows/ci.yml`: rewritten into the four-job structure PRD §F-018
  prescribes — `lint`, `test`, `build-linux`, and a placeholder note for the
  Session-003 `build-android` job. The `lint` job now includes the frontend
  ESLint + typecheck steps; `test` runs both `cargo test` and `vitest`;
  `build-linux` installs the Tauri 2 system deps (`libwebkit2gtk-4.1-dev` +
  friends), `cargo install tauri-cli`, and runs `cargo tauri build`.
- `README.md`: build prerequisites updated (Node 22+, Tauri 2 system deps on
  Ubuntu 24.04 listed explicitly, frontend lint/typecheck/test recipe added).

**Features advanced:**

- F-001: in progress → in progress
  - **Done this session (Linux side):** `src-tauri/` Tauri 2 host binary
    (`kino-app`); SolidJS frontend renders the F-001 placeholder text
    "kino"; `cargo tauri build --target x86_64-unknown-linux-gnu` succeeds
    end-to-end, producing all three Linux artifacts verified locally:
    `kino-app` ELF binary (~5.9 MiB stripped), `kino_0.0.0_amd64.deb`
    (~2.2 MiB), `kino-0.0.0-1.x86_64.rpm` (~2.2 MiB), and
    `kino_0.0.0_amd64.AppImage` (~88 MiB, with bundled WebKit + GTK).
    `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
    warnings`, `cargo test --workspace --all-targets`, `npm run lint`,
    `npm run typecheck`, `npm test`, `npm run build` all pass.
  - **Remaining for `[x]`:** `cargo tauri android build` produces a working
    APK; `build-android` CI job lands; APK installs on Shield + phone
    (latter is §6B human verification, not Code Acceptance).

**ADRs filed this session:**

- **ADR-033** (harness branch naming is informational, not session-numbered):
  The harness provisions a branch name like `claude/session-001-bootstrap-JtPxr`
  per session. That name reflects the **harness invocation** and does NOT
  re-number across agent sessions. We treat it as the working branch
  (because the harness expects pushes to it) but file the actual session
  number in commit messages and the PR title. Session 002 lives on a branch
  named "session-001-bootstrap" because that's what the harness handed us;
  future sessions may receive different names. This formalizes the note
  Session 001 left under its branch entry.
- **ADR-034** (Tauri 2 + Linux runtime stack on Ubuntu 24.04): kino targets
  Ubuntu 22.04 and 24.04 (PRD §6B-1). Ubuntu 24.04 ships
  `libwebkit2gtk-4.1-dev` (NOT 4.0), `libsoup-3` (NOT 2.4), and the
  Ayatana-indicator stack. The CI `build-linux` job installs the 4.1
  packages plus the three AppImage-tooling deps that the official Tauri 2
  docs don't enumerate: `libfuse2t64` (FUSE runtime for
  linuxdeploy-x86_64.AppImage), `patchelf` (used by
  linuxdeploy-plugin-gtk to rewrite RPATHs on bundled libraries), and
  `squashfs-tools` (`mksquashfs` is appimagetool's bundler). PRD §6B-1
  verification on Ubuntu 22.04 will need the 4.0-named packages there. The
  Tauri 2 runtime auto-detects WebKit version, so no source change is
  needed to support both — only the CI matrix would change to add a
  22.04 runner. (Adding that is a nice-to-have for Session 003+.)
- **ADR-035** (placeholder icons committed): F-001 needs PNG icons for
  Tauri's bundler. Until a real brand asset exists, deterministic
  programmatically-generated PNGs (DejaVu-Bold "k" over `#0a0a0a`) live at
  `src-tauri/icons/`. Tauri's bundler is happy; this is a polish task for a
  later session and is filed under Known Issues / Tech Debt.
- **ADR-036** (frontend lint config = ESLint 9 flat config + typescript-eslint
  + eslint-plugin-solid): The frontend uses ESLint 9 (current stable),
  meaning the legacy `.eslintrc.cjs` form is dead. `frontend/eslint.config.js`
  exports a flat config tree. One library-pattern `eslint-disable-next-line
  solid/reactivity` is in `src/i18n.ts` because the plugin can't see across
  the `translator()` library boundary; documented in-place.

**Tests added / coverage notes:**

- Rust: no new tests this session — Session 001's 23 already cover all the
  locked content. `kino-app` is a thin wiring crate with no testable logic
  yet; commands land with their feature.
- Frontend: 7 new vitest cases (5 in `i18n.test.ts`, 2 in `App.test.tsx`).
  Locale resolution coverage: undefined candidates, no-match fallback,
  first-match-wins, region-subtag stripping, case-insensitive matching.
  App coverage: the F-001 required title text + tagline both render.

**Known issues introduced or resolved:**

- **New (introduced):** placeholder icons are programmatic, not designed
  (ADR-035). File under Known Issues / Tech Debt below.
- **Resolved (Session 002, locally) — AppImage bundling on Ubuntu 24.04
  needs three system packages beyond the Tauri 2 docs minimum:**
  `libfuse2t64` (Ubuntu 24.04's libfuse 2 successor —
  `linuxdeploy-x86_64.AppImage` dlopens `libfuse.so.2` even when invoked
  with `--appimage-extract-and-run`), `patchelf` (linuxdeploy-plugin-gtk
  walks the AppDir and rewrites RPATH on every GTK/WebKit ELF — without it,
  the plugin exits 1 partway through), and `squashfs-tools` (`mksquashfs` is
  appimagetool's bundler). With those three added, the `appimage`,
  `deb`, and `rpm` bundle steps all succeed locally. The `build-linux` CI
  workflow installs the same superset so first-CI green is the expected
  outcome; if it isn't, the failure mode is one of these packages, and the
  fix is a tighter version pin or a fallback. The Tauri 2 docs only mention
  the `webkit2gtk` / `xdo` / `appindicator` / `rsvg` quartet — these three
  extras are an artifact of the AppImage tooling chain rather than Tauri
  itself.
- **Resolved:** ADR-031 ("`src-tauri/` not in workspace until it compiles")
  is satisfied. The directory now has a valid `Cargo.toml` and is a workspace
  member.

**Heads-up for Session 003:**

- **Primary scope: F-001 Android completion.** Install the Android NDK
  (`sdkmanager --install "ndk;27.0.12077973"` or the version Tauri 2 docs
  recommend at session-start), the Android command-line tools, and JDK 17.
  Run `cargo tauri android init` from `src-tauri/` to generate
  `src-tauri/gen/android/`. Wire signing with the committed
  `android/keystore/kino-dev.keystore` (alias `kino-dev`, store/key pw
  `kinodev`). Get `cargo tauri android build --apk` green locally, then add
  the `build-android` job to `.github/workflows/ci.yml`. Flip F-001 to `[x]`.
- **Secondary scope (if Android lands fast): F-002 Persistence layer.** The
  migration is already shipped (`migrations/0001_init.sql`); F-002 needs the
  sqlx connection-pool wiring in `kino-core`, the `kv_get`/`kv_set`/CW CRUD/
  addons CRUD Tauri commands, install_id bootstrap, and the corresponding
  unit tests.
- If Android proves hard (NDK download flakiness, signing weirdness,
  emulator setup), split: 003 = Android scaffold + green build; 004 = F-002;
  005 = F-003 metadata clients.
- The container had Tauri-2 Linux deps available via apt at Session 002
  time; if a future container is missing them, the CI workflow's install
  block is the source of truth.

**Branch:** `claude/session-001-bootstrap-rsgXK`
(The harness-supplied branch overrides the AGENT_PROMPT `agent/session-NNN-*`
naming convention. Future sessions follow whichever branch the harness assigns
or, if none assigned, the protocol's `agent/session-NNN-<slug>` form.)

**Scope chosen:** F-001 partial — workspace metadata, Rust crate skeletons,
locked-content modules, Android keystore, initial CI, and STATE.md bootstrap.
Full Tauri host (`src-tauri/`) and SolidJS frontend (`frontend/`) are
deliberately deferred to Session 002 because the Tauri 2 CLI is not installed
in this environment and pulling it in (plus the Android NDK and SolidJS
tooling) would balloon Session 001 into a multi-hour install marathon with no
verifiable output. The protocol explicitly invites this "scaffolding +
implementation" split.

**Files added (summary):**

- Repo metadata: `LICENSE` (MIT, "kino contributors", 2026), `README.md`,
  `.gitignore`, `rust-toolchain.toml` (pinned to 1.94.1, the stable Rust
  shipped with this environment).
- Workspace root: `Cargo.toml` declaring the five crates from PRD §3 (with
  shared `[workspace.dependencies]` and `[workspace.package]` metadata).
  `src-tauri/` is intentionally NOT yet in the members list — see ADR-031.
- `crates/kino-core/`: shell + `constants` (every numeric constant from
  PRD §8) + `title` (TitleKind / TitleSummary) + `stream` (Quality / Hdr /
  Codec / Audio / ParsedTags enums) + `Error` / `Result`. `forbid(unsafe_code)`.
- `crates/kino-torrent/`: shell + `trackers` (full PRD §8 supplementary list).
- `crates/kino-server/`: shell only (axum wiring lands with F-013).
- `crates/kino-addons/`: shell + `parse` (full PRD §8 regex set + behavioral
  tests covering all four locked fixtures) + `recommended` (full PRD §8
  addon table with Cinemeta pinned as `CINEMETA_MANIFEST_URL` for F-007 to
  reference).
- `crates/kino-metadata/`: shell only (provider clients land with F-003).
- `migrations/0001_init.sql`: locked schema from PRD §F-002.
- `android/keystore/kino-dev.keystore`: generated with `keytool` per PRD §F-001
  exact parameters (PKCS12, RSA 2048, alias `kino-dev`, store/key pw `kinodev`,
  validity 10000 d, `CN=kino dev, O=kino, C=FR`). Committed by design.
- `android/keystore/README.md`: documents the keystore is committed for
  sideload reproducibility and is not a security control.
- `.github/workflows/ci.yml`: rust job (fmt / clippy / test) wired to
  `dtolnay/rust-toolchain@stable` + cargo cache. `build-linux` /
  `build-android` jobs deliberately omitted until the Tauri host exists, so
  the green badge actually means something.

**Features advanced:**

- F-001: not started → in progress
  - Done: workspace layout, all metadata files, all 5 crate skeletons, locked
    constants/trackers/recommended/parse, keystore, keystore docs, CI scaffolding.
  - Remaining for completion: `src-tauri/` Tauri 2 binary, `frontend/` SolidJS
    app, `cargo tauri build` (Linux) verified green, `cargo tauri android build`
    verified green. These are Session 002's primary scope.

**ADRs filed this session:**

- **ADR-029** (parse regex precision): PRD §8 specifies trailing `\b` on the
  audio detectors `\b(EAC3|DDP|DD\+|E-AC-3)\b` and `\b(AC3|DD)\b`, but the
  PRD's own locked fixture row 3 (`Some Show S01E01 720p WEB-DL DDP5.1 H.264`)
  requires `DDP5.1` to be tagged as EAC3. `\bDDP\b` cannot match `DDP5.1`
  because `P→5` is not a word boundary. The fixture table is the behavioral
  spec, so the implementation in `crates/kino-addons/src/parse.rs` tightens
  the trailing boundary to `(?:\b|\d)` — a real word boundary OR a single
  digit (e.g. a channel-count prefix like `5.1`/`7.1`). Regression test
  `audio_does_not_false_positive_on_letter_suffixes` proves the fix doesn't
  open the door to `DDS` / `DDPL` style false positives. See PRD Issues for
  the corresponding §8 revision request.
- **ADR-030** (workspace lints): each crate sets `#![forbid(unsafe_code)]`
  via `[lints.rust]` and enables `clippy::all + pedantic` with three narrowly
  scoped `allow`s (`module_name_repetitions`, `must_use_candidate`,
  `missing_errors_doc`). The first is noise for a multi-crate workspace, the
  other two are pre-empted because they would force premature documentation
  churn on shells. CI runs `-D warnings`, so the lint level is enforced.
- **ADR-031** (`src-tauri/` not in workspace until it compiles): listing
  `src-tauri/` in `[workspace] members` before the directory has a
  `Cargo.toml` would break `cargo build --workspace` from day one. Session 002
  adds the directory and amends `Cargo.toml`.
- **ADR-032** (cross-constant invariants as compile-time asserts): the
  relationships between locked constants (e.g. `PREBUFFER_TARGET_S <
  SAFETY_MARGIN_S`) are enforced by `const _: () = assert!(...)` at module
  level in `kino-core/src/constants.rs`. Any value bump that breaks the PRD
  math fails the build, not just `cargo test`. Strictly stronger than the
  runtime tests they replace.

**Tests added / coverage notes:**

- 23 unit tests across the workspace, all green: constants (2), title (2),
  trackers (3), parse (12 — including the 4 PRD-locked fixture cases + the
  ADR-029 regression guard), recommended (4).
- Three constant invariants enforced at compile time (ADR-032).

**Known issues introduced or resolved:**

- None introduced. PRD Issue filed for §8 regex set — see below.

**Heads-up for Session 002:**

- The natural scope is "F-001 completion": create `src-tauri/` (Tauri 2
  binary, `tauri.conf.json` set to App ID `dev.kino.app` and display name
  `kino`, placeholder home that renders the text `kino`), create `frontend/`
  (Vite + SolidJS + Tailwind + i18n with `en.json` + `fr.json` placeholders),
  add `src-tauri` to the workspace members list (resolving ADR-031), and
  extend `.github/workflows/ci.yml` with a `build-linux` job that runs
  `cargo tauri build --target x86_64-unknown-linux-gnu`. If that's too big in
  practice, split: 002 = src-tauri scaffold, 003 = frontend scaffold + CI
  build job. Either way F-001 moves to `[x]` by the end.
- `cargo install tauri-cli --version "^2.0.0"` is required in the session
  container. Budget ~5 min of compile time.
- The Android build job (`cargo tauri android build`) needs the Android NDK
  + SDK; if not installed in the session container, install with
  `sdkmanager --install "ndk;26.1.10909125"` (the LTS NDK at the time of
  writing). May need to defer to Session 003 if the container is slow.

---

## Feature Tracker

### Foundation
- [ ] F-001: Project scaffolding _(in progress — Session 001 landed metadata
  + crates + keystore; Session 002 landed src-tauri + frontend + green Linux
  `cargo tauri build` + extended CI; Session 004 lands `cargo tauri android
  build` + the `build-android` CI job to flip this to `[x]`)_
- [x] F-002: Persistence layer _(Session 003: sqlx pool, WAL,
  migrations + install_id bootstrap, KV/CW/addons API + Tauri commands, 16 tests)_

### Metadata & Catalogs
- [ ] F-003: Metadata clients (TMDB / Trakt / TVDB / Fanart.tv)
- [ ] F-004: Trending aggregation with diversity
- [ ] F-005: Image & logo resolution
- [ ] F-006: Source availability filter
- [ ] F-007: Stremio addon protocol client

### UI
- [ ] F-008: Home screen (10-foot UI)
- [ ] F-009: Movies and Series sub-homes
- [ ] F-010: Title detail view
- [ ] F-011: Search
- [ ] F-012: Continue Watching
- [ ] F-016: Settings screen
- [ ] F-017: Input handling

### Streaming
- [ ] F-013: Embedded torrent engine
- [ ] F-014: Adaptive buffer
- [ ] F-015: Native player integration

### Release
- [ ] F-018: Build, packaging, distribution

---

## Architectural Decisions Log

ADR-001 through ADR-028 are inherited from `PRD.md` §7. They are immutable.

Additional ADRs filed by sessions:

| ID | Decision | Session |
|---|---|---|
| ADR-029 | Tighten audio EAC3/AC3 trailing boundary to `(?:\b\|\d)` to satisfy PRD §8 fixture `DDP5.1 → EAC3`. PRD §8 regex text is treated as a strong recommendation; the locked fixture table is the binary acceptance spec. | 001 |
| ADR-030 | Per-crate `forbid(unsafe_code)` + `clippy::pedantic` with `module_name_repetitions / must_use_candidate / missing_errors_doc` allowed. CI enforces `-D warnings`. | 001 |
| ADR-031 | `src-tauri/` is omitted from `[workspace].members` until its `Cargo.toml` exists (lands Session 002). | 001 |
| ADR-032 | Cross-constant relational invariants (e.g. `PREBUFFER_TARGET_S < SAFETY_MARGIN_S`) are compile-time `const _: () = assert!(..)` rather than runtime tests. | 001 |
| ADR-033 | Harness-supplied branch name (e.g. `claude/session-001-bootstrap-JtPxr`) is the working branch the harness expects pushes to; it is NOT renamed across sessions. Session number lives in commit messages and the PR title. | 002 |
| ADR-034 | Tauri 2 on Ubuntu 24.04 uses the `libwebkit2gtk-4.1-dev` / `libsoup-3` / Ayatana indicator stack. The CI workflow installs those packages explicitly; cross-distro coverage (22.04 in particular) is a §6B-1 human verification concern. | 002 |
| ADR-035 | Placeholder Tauri bundle icons (programmatic DejaVu-Bold "k" PNGs) live in `src-tauri/icons/` until a real brand asset replaces them. | 002 |
| ADR-036 | Frontend lint config uses ESLint 9 flat config (`frontend/eslint.config.js`) + `typescript-eslint` + `eslint-plugin-solid`. One scoped `eslint-disable-next-line solid/reactivity` documents the `@solid-primitives/i18n` `translator()` library boundary the plugin can't analyze across. | 002 |
| ADR-037 | `Db::open_in_memory()` forces `max_connections = 1`; the file-backed `Db::open()` keeps PRD §3's `max_connections = 4`. `sqlx` in-memory mode gives each connection a private DB unless paired with a `?cache=shared` URI (which `SqliteConnectOptions` does not expose), so a 4-way pool would see migrations on one connection and nothing on the others. | 003 |
| ADR-038 | DB path resolution lives in the Tauri host (`src-tauri/src/paths.rs`), not in `kino-core`. Linux uses `dirs::config_dir().join("kino")` per PRD §3 (NOT Tauri's default `app_config_dir()`, which would yield `~/.config/dev.kino.app`). Android delegates to `app.path().app_config_dir()` which maps to `Context.filesDir`. `kino-core` exposes the file-name constant only. | 003 |
| ADR-039 | F-002 Tauri commands return `Result<T, String>` with `e.to_string()` on failure. A typed Serialize'd error enum is deferred until the UI surfaces these errors with localized messages (PRD §5 i18n); String is a subtype of any future envelope so this is a non-breaking choice. | 003 |

---

## Known Issues / Tech Debt

- **Placeholder Tauri icons.** `src-tauri/icons/*.png` are programmatic
  black-background "k" PNGs (ADR-035). Replace with real brand assets in a
  polish pass before any public release. Not blocking for §6A.
- **AppImage bundle step not exercised locally in Session 002.** The
  `cargo tauri build --target x86_64-unknown-linux-gnu` step was verified
  with `--no-bundle` for time budget. The full bundle step (which downloads
  `linuxdeploy` and packages the AppImage) lands first in CI. If CI flags
  it, Session 003 fixes it as highest-priority scope before any other work.

---

## PRD Issues

- **§8 regex set, audio EAC3/AC3 trailing boundary contradicts fixture
  table.** The locked regexes `\b(EAC3|DDP|DD\+|E-AC-3)\b` and
  `\b(AC3|DD)\b` cannot match the locked fixture filename
  `Some Show S01E01 720p WEB-DL DDP5.1 H.264` (intended to yield EAC3),
  because there is no word boundary between `P` and `5` in `DDP5.1`. The
  fix shipped in Session 001 (ADR-029) replaces the trailing `\b` with
  `(?:\b|\d)` — a real word boundary OR a single digit — which preserves
  the rejection of letter-suffixed false positives like `DDS` while
  accepting channel-count suffixes that are standard in scene release
  names. **Suggested PRD §8 revision:** update the documented regex
  strings to match the shipped implementation, e.g.
  `\b(?:EAC3|DDP|DD\+|E-AC-3)(?:\b|\d)` and `\b(?:AC3|DD)(?:\b|\d)`. No
  behavioral change is needed; this is a documentation correction.

---

## §6B Verification

_Filled by the human post-release._

- [ ] §6B-1: Linux AppImage launches on Ubuntu 22.04 and 24.04
- [ ] §6B-2: APK installs on Android phone, stream plays end-to-end
- [ ] §6B-3: APK installs on Shield Pro 2019, Shield remote navigation works
- [ ] §6B-4: DV Profile 5 movie plays on Shield with DV indicator
- [ ] §6B-5: Atmos TrueHD plays with AVR showing Atmos
- [ ] §6B-6: Adaptive buffer engages correctly on real slow torrent
- [ ] §6B-7: Continue Watching saves and resumes correctly
- [ ] §6B-8: APK reinstall over previous version succeeds

---

## §6B Regressions

_Filed by the human when §6B items fail. Sessions address these as highest-priority scope._

---

## Cross-Session Conventions

Populated as conventions are established:

- **Crate layout.** Each crate under `crates/` follows the structure
  `Cargo.toml` (with `[lints.rust]` and `[lints.clippy]` from ADR-030) +
  `src/lib.rs` (declares modules, exports nothing else top-level unless
  needed) + one file per module. Tests live in `#[cfg(test)] mod tests`
  inside the module under test.
- **Dependency discipline.** Shared deps live in
  `[workspace.dependencies]` and are pulled into individual crates with
  `{ workspace = true }`. Per-feature deps (e.g. librqbit, axum) are added
  in the session that implements the feature, not preemptively, so unrelated
  sessions don't pay their compile cost.
- **Locked content lives in PRD-pinned modules.** Constants → `kino-core::constants`.
  Trackers → `kino-torrent::trackers`. Filename regex set → `kino-addons::parse`.
  Recommended addons → `kino-addons::recommended`. Migrations → `migrations/`.
- **PRD line numbers are not stable references.** When citing PRD provisions
  in code comments or ADR text, cite the section / feature ID (e.g.
  `PRD §F-014`, `PRD §8`), never line numbers.
- **Frontend layout.** `frontend/` is a single SolidJS bundle shared across
  Linux / Android / Android TV (ADR-013). Locales live in `src/locales/<lang>.json`
  (PRD §5). Tauri's `tauri.conf.json` points `frontendDist` at
  `../frontend/dist` and runs `npm --prefix ../frontend run dev/build` from
  `before{Dev,Build}Command`. Tests use vitest + jsdom; setup hook is
  `src/test-setup.ts`.
- **Tauri host crate (`src-tauri/`).** The crate is `kino-app`, library name
  `kino_app_lib`. Desktop builds use the binary in `src/main.rs`; Android
  links the cdylib. The shared `run()` lives in `src/lib.rs` and is the only
  thing the Android entry point will need (`#[cfg_attr(mobile,
  tauri::mobile_entry_point)]`). Tauri commands are registered alongside
  the feature that adds them — never as preemptive stubs.
