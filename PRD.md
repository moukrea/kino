# kino — Product Requirements Document

**Version:** 1.0 (locked)
**Status:** Source of truth for v1. Immutable except by human edit.
**License:** MIT
**Default branch:** main
**Distribution:** Sideload (Android/AndroidTV) + direct download (Linux). No app stores in v1.

---

## 1. Vision

A Tauri 2 multiplatform streaming client that consumes the Stremio addon ecosystem, embeds its own torrent engine with adaptive piece-deadline buffering, and presents a 10-foot UI tuned for Android TV, mobile Android, and desktop Linux.

Differentiators over Stremio:

- Adaptive prebuffer computed from measured download rate vs. file bitrate, recomputed continuously during playback. No fixed buffer size.
- Native ExoPlayer integration with full Dolby Vision (profiles 5 and 8.1), HDR10/+, and audio passthrough (TrueHD / DTS-HD MA / Atmos / E-AC3 JOC) on Android.
- libmpv on Linux for codec coverage parity and HDR.
- Strict source-availability filter: catalogs only show titles for which at least one configured Stremio addon returns streams.
- No Real-Debrid required, no degraded UX on Android (Stremio mobile cannot override its streaming server; kino has full control because the server is in-process).

## 2. Scope

### In scope (v1)

- Platforms: Android (touchscreen primary), Android TV (D-pad + gamepad primary), Linux x86_64 (keyboard + mouse primary)
- Single-user, single-device, local state only
- Stremio addon protocol as the sole stream source layer
- Metadata aggregation from TMDB, Trakt, TVDB
- Imagery (poster, backdrop, logo, clearart) aggregated from Fanart.tv, TMDB, and TVDB with cross-provider and cross-language fallback chain (see F-005)
- Continue Watching, local
- Search across metadata providers with live results
- Source availability filter
- Embedded torrent engine (librqbit) with adaptive buffer
- Native player per platform (ExoPlayer/Media3 on Android, libmpv on Linux)
- GitHub Actions producing installable artifacts for all three platforms, signed with a stable keystore committed in repo for sideload reproducibility

### Out of scope (v2+)

- macOS, Windows, iOS
- Multi-profile, accounts, server-side sync
- Real-Debrid / Premiumize / AllDebrid integrations
- Live TV, IPTV
- Cast / DLNA / AirPlay
- Encrypted-at-rest credentials
- Telemetry of any kind
- Smart TV native builds (Tizen, WebOS)
- Daemon mode / remote server
- External player intent delegation (no MX Player / VLC handoff)
- App store distribution (Play Store, F-Droid)
- Library / Watchlist management
- Voice search
- Cast/sharing playback state across devices

## 3. Architecture

### Stack (locked)

| Layer | Choice | Locked version baseline |
|---|---|---|
| Host | Tauri | 2.1+ |
| Backend language | Rust | latest stable at Session 001; MSRV = that version, no chasing |
| Frontend | SolidJS | 1.9+ |
| Frontend build | Vite | 5+ |
| Frontend styling | TailwindCSS | 3+ |
| Torrent engine | `librqbit` | latest stable at Session 001 |
| Local HTTP server | `axum` | 0.7+ |
| Outbound HTTP | `reqwest` | 0.12+ |
| Persistence | SQLite via `sqlx` | sqlx 0.8+, WAL mode |
| Logging (Rust) | `tracing` + `tracing-subscriber` | latest |
| Errors (Rust) | `thiserror` (libs) + `anyhow` (bin) | latest |
| Player (Android) | ExoPlayer / Media3 | 1.4+ |
| Player (Linux) | libmpv via `libmpv-rs` | libmpv 0.36+ |
| i18n (frontend) | `@solid-primitives/i18n` | latest |
| Test (Rust) | built-in `cargo test` | n/a |
| Test (frontend) | vitest | latest |

No alternative shall be selected for any of the above. Substitution requires a human PRD revision.

### Workspace layout (locked)

```
kino/
├── PRD.md                          # Source of truth (this document)
├── STATE.md                        # Agent state (created Session 001)
├── LICENSE                         # MIT (created Session 001)
├── README.md                       # Created Session 001
├── .gitignore                      # Created Session 001
├── Cargo.toml                      # Workspace root
├── Cargo.lock
├── rust-toolchain.toml             # Pin Rust version
├── crates/
│   ├── kino-core/                  # Shared types, settings, install_id, db
│   ├── kino-torrent/               # librqbit wrapper + adaptive buffer scheduler
│   ├── kino-server/                # axum HTTP server for player consumption
│   ├── kino-addons/                # Stremio addon protocol client
│   └── kino-metadata/              # TMDB / Trakt / TVDB / Fanart.tv clients
├── src-tauri/                      # Tauri host app (binary)
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   └── src/
├── frontend/                       # SolidJS app
│   ├── package.json
│   ├── vite.config.ts
│   ├── tailwind.config.js
│   ├── tsconfig.json
│   ├── index.html
│   └── src/
│       ├── locales/
│       │   ├── en.json
│       │   └── fr.json
│       └── ...
├── android/
│   ├── keystore/
│   │   ├── kino-dev.keystore       # Stable sideload keystore (see F-018)
│   │   └── README.md               # Documents the keystore parameters
│   └── player-plugin/              # Native Kotlin ExoPlayer plugin
├── migrations/                     # sqlx migrations
└── .github/
    └── workflows/
        ├── ci.yml
        └── release.yml
```

### Process model

Single Tauri process. The Rust backend:

- Exposes Tauri commands (IPC) for state, settings, addon ops, metadata, playback control
- Runs the axum HTTP server bound to `127.0.0.1:0` (OS-assigned port, captured in shared state)
- Owns the librqbit session and the buffer scheduler
- Owns the SQLite handle (single connection pool, size 4, WAL mode)

The frontend is a single SolidJS bundle shared across all three targets. UI variants are toggled by a runtime "input profile" (touch / dpad / kbm), not separate bundles.

### Playback data flow

1. User selects a title. Frontend calls Tauri command `get_streams(type, id)`.
2. Backend queries all enabled Stremio addons in parallel. Returns ranked stream list (quality desc, then seeders desc, then size desc).
3. User picks a stream. Frontend calls `start_playback(stream, resume_position)`.
4. Backend adds the torrent (or magnet) to librqbit, identifies the largest video file in the torrent, computes initial prebuffer target (F-014), waits until satisfied.
5. Backend issues a UUID v4 token and returns `http://127.0.0.1:PORT/stream/<token>` to the frontend along with the file's MIME hint and parsed metadata (codec, HDR, audio).
6. Frontend launches the platform player with this URL plus `resume_position`:
   - **Android**: starts a native `PlayerActivity` (fullscreen, Kotlin, owns ExoPlayer). Activity communicates back to the Tauri plugin via local IPC.
   - **Linux**: instantiates the in-window libmpv player, controls overlaid via SolidJS.
7. Player emits position updates every 5s and on every state change. Backend persists for Continue Watching and feeds the adaptive buffer scheduler.
8. On stop/exit, backend invalidates the token and (optionally) keeps the torrent active per cache policy.

### Storage layout

| Data | Linux | Android |
|---|---|---|
| App config + DB | `$XDG_CONFIG_HOME/kino/` (default `~/.config/kino`) | `Context.filesDir` |
| Torrent cache (default) | `$XDG_CACHE_HOME/kino/torrents/` | external storage if writable, else internal |
| Logs | `$XDG_STATE_HOME/kino/logs/` | `Context.cacheDir/logs/` |

Cache path is user-configurable. On Android TV, first-launch wizard recommends pointing the cache to a connected USB drive if one is detected.

## 4. Features

Each feature has machine-verifiable acceptance criteria (CC validates) and may have human-verification criteria (human validates post-merge, see §6B).

### F-001: Project scaffolding

Create the full workspace layout from §3. Outputs:

- `LICENSE` containing the MIT license, copyright holder "kino contributors", current year
- `README.md` with project summary, build instructions, contribution note pointing at `PRD.md`
- `.gitignore` for Rust, Node, Tauri, Android, OS artifacts
- `rust-toolchain.toml` pinning the stable Rust version selected at Session 001
- Workspace `Cargo.toml` declaring the five crates listed in §3
- `src-tauri/` with a working Tauri 2 binary that shows a placeholder home screen on all three platforms
- `frontend/` with a working SolidJS + Vite + Tailwind setup that builds and is loaded by Tauri
- `android/keystore/kino-dev.keystore` generated with these exact parameters (committed):
  - Alias: `kino-dev`
  - Key password: `kinodev`
  - Store password: `kinodev`
  - Validity: 10000 days
  - CN=`kino dev`, O=`kino`, C=`FR`
  - Algorithm: RSA 2048
- `android/keystore/README.md` explaining the keystore is committed by design for sideload reproducibility and is not a security control

Naming, copyright, and metadata:

- App ID (Android): `dev.kino.app`
- App display name: `kino`

**Code acceptance:**

- `cargo tauri build` succeeds on Linux producing an executable
- `cargo tauri android build` produces a working APK
- App launches and shows a placeholder home screen with the text "kino" on all targets
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` all pass
- Frontend `npm run build`, `npm run typecheck`, `npm run lint`, `npm test` all pass

**Human verification:**

- APK installs on Shield Pro 2019 via `adb install` and launches
- APK installs on a real Android phone (arm64) and launches
- Linux binary launches on Ubuntu 22.04 and 24.04

### F-002: Persistence layer

SQLite via `sqlx` with embedded migrations in `migrations/`. WAL mode enabled. Connection pool size 4.

Schema (migration `0001_init.sql`):

```sql
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE addons (
  id TEXT PRIMARY KEY,
  manifest_url TEXT NOT NULL UNIQUE,
  enabled INTEGER NOT NULL DEFAULT 1,
  installed_at INTEGER NOT NULL,
  manifest_json TEXT NOT NULL,
  display_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE continue_watching (
  title_id TEXT NOT NULL,
  type TEXT NOT NULL CHECK(type IN ('movie','series')),
  season INTEGER NOT NULL DEFAULT 0,
  episode INTEGER NOT NULL DEFAULT 0,
  position_s REAL NOT NULL,
  duration_s REAL NOT NULL,
  last_played_at INTEGER NOT NULL,
  meta_json TEXT NOT NULL,
  PRIMARY KEY (title_id, season, episode)
);
CREATE INDEX idx_continue_watching_last_played ON continue_watching(last_played_at DESC);

CREATE TABLE response_cache (
  key TEXT PRIMARY KEY,
  payload_json TEXT NOT NULL,
  etag TEXT,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_response_cache_expires ON response_cache(expires_at);

CREATE TABLE stream_availability (
  title_id TEXT NOT NULL,
  type TEXT NOT NULL,
  source_id TEXT NOT NULL,
  has_streams INTEGER NOT NULL,
  checked_at INTEGER NOT NULL,
  PRIMARY KEY (title_id, type, source_id)
);
CREATE INDEX idx_stream_availability_checked ON stream_availability(checked_at);

CREATE TABLE recent_searches (
  query TEXT PRIMARY KEY,
  searched_at INTEGER NOT NULL
);
```

Bootstrap on first launch: `settings.install_id` = UUID v4 (used in F-004 seed).

**Code acceptance:**

- DB created on first launch at the path defined in §3
- Migrations apply cleanly and idempotently (running twice yields no error)
- Connection pool initialized, WAL mode confirmed via `PRAGMA journal_mode`
- Tauri commands implemented for KV settings (`kv_get`, `kv_set`), CW CRUD, addons CRUD
- Unit tests cover migration round-trip, KV operations, CW upsert behavior

### F-003: Metadata clients

HTTP clients in `kino-metadata` crate for:

- **TMDB** (`api.themoviedb.org/3`): trending, search, find, movie, tv, configuration
- **Trakt** (`api.trakt.tv`): trending movies, trending shows, search
- **TVDB** (`api4.thetvdb.com/v4`): movies, series, search
- **Fanart.tv** (`webservice.fanart.tv/v3`): images for movies and shows

Common client config (locked):

- Default request timeout: 10s
- Retry policy: 3 attempts with exponential backoff (1s, 2s, 4s) on 5xx and 429
- User agent: `kino/<version> (+https://github.com/<repo>)` (repo URL templated at build time)
- ETag handled where the provider supports it; stored in `response_cache.etag`
- All responses cached in `response_cache` with TTLs defined in §8

API keys live in `settings` (`tmdb_api_key`, `trakt_api_key`, `tvdb_api_key`, `fanart_api_key`). App ships with no keys. First launch presents a setup wizard.

Provider availability rules:

- TMDB is required. Without it, home and search are empty with a clear "Configure TMDB key" message.
- Trakt absence: trending merge falls back to TMDB + TVDB only. No error.
- TVDB absence: same fallback logic.
- Fanart.tv absence: image resolution skips that source in the fallback chain. No error.

**Code acceptance:**

- One client per provider in `kino-metadata`, each with its own module
- Each client exposes a `test_credentials() -> Result<(), Error>` method
- Tauri commands: `test_tmdb()`, `test_trakt()`, `test_tvdb()`, `test_fanart()`
- 429 and 5xx responses retry with the locked backoff
- Unit tests using `wiremock` cover happy path, 429 retry, 500 retry, timeout

### F-004: Trending aggregation with diversity

Algorithm (locked):

1. Fetch from each enabled provider with key:
   - TMDB: `/trending/movie/week`, `/trending/tv/week` (limit 100)
   - Trakt: `/movies/trending`, `/shows/trending` (limit 100)
   - TVDB: filter sorted by score, last 90 days (limit 100)
2. Deduplicate by IMDb ID. Items without IMDb ID resolved via TMDB `/find` when possible, else dropped.
3. Normalize rank from each provider to `[0..1]` where rank 0 = best.
4. Compute weighted score: `score = 0.45 × trakt + 0.35 × tmdb + 0.20 × tvdb`. Missing rank from a provider treated as 0.5 (neutral). Lower score = better.
5. Split into two pools:
   - **Top trending**: items in the top quartile of merged score
   - **Hidden gems**: items NOT in top trending, with `rating > 7.5` AND `popularity_rank < median(popularity_rank)` of the fetched set (popularity from TMDB `popularity` field)
6. Final list of 50: alternating pattern `[T, T, T, G, G]` repeating until 50 reached. If a pool runs out, fill from the other.
7. Apply daily shuffle:
   - `seed = SHA256(YYYY-MM-DD UTC || settings.install_id)`
   - Use seed to deterministically permute the final list via Fisher-Yates with a seeded PRNG (`rand_chacha::ChaCha20Rng::from_seed`)

**Code acceptance:**

- `get_trending(type, locale) -> Vec<TitleSummary>` Tauri command, returns 50 items max
- Two invocations within the same UTC day return identical ordering
- Invocations on consecutive UTC days return permutations with Kendall tau correlation < 0.7
- Two installations with different install_ids return different orderings on the same day
- Unit tests cover the merge, the pool split, the alternation, the seeded shuffle

### F-005: Image & logo resolution

For each title, resolve poster, backdrop, logo, clearart, and summary in the user's preferred language with a full cross-provider and cross-language fallback chain.

#### Image fallback chain (poster, backdrop, logo, clearart)

Locked algorithm. Within each language tier, providers are tried in this fixed order: **Fanart.tv → TMDB → TVDB**. Language tiers are tried in this order:

1. **Tier 1 — primary language**: Fanart.tv (primary) → TMDB (primary) → TVDB (primary)
2. **Tier 2 — first configured fallback language**: Fanart.tv → TMDB → TVDB
3. **Tier 3 — second configured fallback language**: Fanart.tv → TMDB → TVDB
4. **Tier 4 — third configured fallback language**: Fanart.tv → TMDB → TVDB
5. **Tier 5 — any other language available** (each provider returns whatever it has): Fanart.tv → TMDB → TVDB
6. **Tier 6 — local placeholder asset** shipped with the app

The first non-empty asset returned wins for that image type. Each image type (poster, backdrop, logo, clearart) is resolved independently. It is normal for a title to end up with poster from Fanart.tv tier 1, backdrop from TMDB tier 2, no logo, and clearart from placeholder.

Logo is best-effort across all tiers: many titles have no logo asset at all from any provider. The renderer falls back to a stylized text title when no logo is found.

A provider is skipped entirely (treated as "no asset") if its API key is not configured.

The provider order within a language tier is locked and NOT user-configurable. Only the language fallback chain (tiers 2-4) is user-configurable, in F-016 Language settings (up to 3 fallback languages).

#### Summary text fallback chain

Summary text follows the same language tier structure but uses only providers that serve summaries: **TMDB → TVDB** (Fanart.tv does not serve summary text).

1. Tier 1 — primary language: TMDB → TVDB
2. Tier 2 — first fallback language: TMDB → TVDB
3. Tier 3 — second fallback language: TMDB → TVDB
4. Tier 4 — third fallback language: TMDB → TVDB
5. Tier 5 — any other language: TMDB → TVDB
6. Tier 6 — empty string

The first non-empty summary wins. A provider is skipped if its API key is missing.

#### Caching

Resolved URL sets and summary text are cached in `response_cache` for 7 days per `(title_id, type, lang_chain_hash)` key. Changing the user's language preferences invalidates the cache on next read.

**Code acceptance:**

- `resolve_artwork(title_id, type, lang_pref: Vec<String>) -> Artwork` Tauri command returns a struct with `poster`, `backdrop`, `logo`, `clearart`, `summary` fields plus a per-field `source` indicator (e.g., `"fanart.tv:en"`, `"tmdb:fr"`, `"placeholder"`) for debugging
- Returned URLs cached for 7 days
- A title with no artwork in any provider returns placeholder URLs for images and empty string for summary without crashing
- A title with missing Fanart.tv key still resolves via TMDB/TVDB across all language tiers
- Unit tests cover: each tier resolving, provider skip on missing key, fallback to placeholder, per-image-type independence (e.g., poster from tier 1, logo from tier 3), summary skipping Fanart.tv

### F-006: Source availability filter

For every catalog item displayed in trending, sub-homes, search results, or addon catalogs, the app must confirm that at least one **enabled** Stremio addon serving streams returns a non-empty stream list.

Implementation (locked):

- Batch availability check fired immediately when a catalog is loaded
- Concurrency cap: 8 in-flight stream requests
- Per-stream-request timeout: 5s
- Result cached in `stream_availability` for 30 minutes
- Tile rendering states:
  - **Loading** (skeleton): default while availability unknown
  - **Available**: rendered once any enabled addon returns ≥ 1 stream
  - **Unavailable** (hidden by default): no addon returned streams
- Setting "Show unavailable titles" (default OFF) toggles unavailable tiles to render with a "no source" badge

**Code acceptance:**

- A catalog of 50 items with mixed availability renders only available tiles within 5s on broadband
- Toggling "show all" reveals unavailable tiles with a badge; toggling off hides them
- `stream_availability` table populated correctly post-check
- Unit tests cover concurrency cap, timeout, cache hit, cache miss

### F-007: Stremio addon protocol client

Crate `kino-addons` implements the public Stremio addon protocol:

| Endpoint | Purpose |
|---|---|
| `GET /manifest.json` | Manifest |
| `GET /catalog/{type}/{id}.json` | Catalog (no pagination) |
| `GET /catalog/{type}/{id}/skip=N.json` | Catalog (paginated) |
| `GET /catalog/{type}/{id}/search={q}.json` | Catalog search |
| `GET /meta/{type}/{id}.json` | Title metadata |
| `GET /stream/{type}/{id}.json` | Streams |
| `GET /subtitles/{type}/{id}.json` | Subtitles |

URL forms supported: `https://.../manifest.json`, `stremio://.../manifest.json` (converted to https).

Manifest validation: presence of `id`, `version`, `name`, `types`, `resources`, `catalogs`. Invalid manifests rejected with a typed error.

Default install: **Cinemeta** (`https://v3-cinemeta.strem.io/manifest.json`) added at first launch as a non-removable addon (user can disable but not delete in v1).

Recommended addons list defined in §8.

**Code acceptance:**

- Tauri commands: `install_addon(url)`, `uninstall_addon(id)`, `list_addons()`, `set_addon_enabled(id, enabled)`, `set_addon_order(id, order)`, `get_recommended_addons()`
- Cinemeta installed automatically on first launch
- Cinemeta cannot be uninstalled (returns a typed error)
- Manifest validation rejects invalid manifests with a typed error
- Unit tests cover protocol calls with `wiremock`

### F-008: Home screen (10-foot UI)

Top-level navigation: **Home**, **Movies**, **Series**, **Search**, **Settings**. Left-hand nav rail. Default collapsed (icons only); expands on focus or hover.

Home composition (locked row order):

1. **Continue Watching** (hidden if empty)
2. **Trending Now** (F-004 top-trending pool)
3. **Hidden Gems** (F-004 hidden-gems pool)
4. **Trending This Week** (TMDB `/trending/{type}/week` only, distinct from merged trending)
5. Catalogs from installed addons, in addon `display_order` then catalog order within each addon

Tile specs (locked):

- Poster aspect 2:3
- Base size: 240×360 px reference, scaled responsively
- Focus state: scale 1.08, soft shadow, border glow
- Focus transition: 150ms ease-out
- Title and year overlaid on focused tile only

Tile metadata expansion: after 600ms of held focus, info overlay slides in from below with title, year, runtime/season info, rating, summary (first 200 chars).

Input: D-pad / touch swipe / keyboard arrows / gamepad d-pad+left-stick. Activate: Enter / A / tap / click.

**Code acceptance:**

- D-pad navigation traverses all rows and tiles
- Empty Continue Watching row is hidden, not shown empty
- Tile focus indicator readable (high contrast, > 2px ring)
- Info overlay appears after 600ms held focus
- Rows lazy-load tiles beyond viewport (virtualization)

**Human verification:**

- Focus indicator readable at 3m distance on Shield + TV
- Navigation feels fluid on Shield remote (no jank, no missed inputs)
- Catalog rows from addons appear under the locked rows

### F-009: Movies and Series sub-homes

Identical structure to Home, filtered to `type=movie` and `type=series` respectively:

- Continue Watching row filters to that type only
- Trending rows filtered to that type
- Addon catalog rows filtered: only catalogs whose addon manifest declares the matching type

**Code acceptance:**

- Switching between Home / Movies / Series is instant (no full reload)
- No movie tile in Series; no series tile in Movies
- Filtered Continue Watching empty state hides the row

### F-010: Title detail view

Full-screen modal overlay with (top to bottom):

- Backdrop image with bottom vignette
- Logo (if available) else stylized title
- Year, runtime, age rating (when known), genres
- IMDb, TMDB, Trakt ratings (only when known)
- Summary in user's primary language with fallback
- Cast row: top 6 with photos (TMDB credits)
- Action buttons in a fixed bar: **Play** / **Resume**, **Mark Watched**
- For movies: list of available streams
- For series: season selector + episode list (with thumbnails, titles, air date, summary truncated to 120 chars, per-episode progress bar)

Stream row contents:

- Quality badge: 4K / 1080p / 720p / SD (parsed per §8)
- HDR badge: DV / HDR10+ / HDR10 (parsed)
- Audio badge: ATMOS / TRUEHD / DTS-HD (parsed)
- Codec hint: H.265 / H.264 / AV1 (parsed)
- Source/addon name
- Seeders count
- File size

Stream sort (locked, descending priority): quality, then seeders, then size.

**Code acceptance:**

- Resume button only present when matching Continue Watching entry exists
- Stream parsing produces correct badges from fixture filenames (§8)
- Episode list shows correct progress for partially-watched episodes
- Back navigation returns focus to the originating tile

### F-011: Search

Top-of-screen search bar, focused on entry.

- Debounced live search: 300ms
- Empty query: "Recent searches" (last 10 from `recent_searches`)
- Result list: mixed movies and series, deduped by IMDb ID, F-006 availability filter applied
- Infinite scroll (page size 20)

Sources:

- TMDB `/search/multi`
- TVDB `/search`
- Trakt `/search`
- IMDb ID detection: if query matches `^tt\d+$`, resolve via TMDB `/find` and jump to title detail

`/` keyboard shortcut focuses search on Linux. Y button on gamepad focuses search from anywhere.

**Code acceptance:**

- First visible result within 500ms after user stops typing on broadband
- Pasting `tt1234567` opens the corresponding title detail directly
- Recent searches persist across app restarts
- Voice search button NOT present in v1

### F-012: Continue Watching

Triggered by player position events.

Rules (locked):

- Save position every 5s during playback
- Save final position on player exit (any reason)
- "Position" = the position at which the player was last in PLAYING or PAUSED state
- Mark completed when exit position > 0.95 × duration
- Completed items auto-removed from Continue Watching after 24h
- Manual remove: long-press (touch), Y button (gamepad), Menu button (D-pad), right-click (mouse)

For series:

- Current episode < 95% watched → row shows current episode at saved position, label "Resume S01E03"
- Current episode ≥ 95% watched AND next episode exists → row shows next episode at position 0, label "Up next: S01E04"
- Current episode ≥ 95% watched AND no next episode → series removed from Continue Watching

**Code acceptance:**

- Resuming starts at saved position within 2s
- Manual remove is immediate and persisted
- Completed items don't reappear in the row
- Series next-episode logic correct in unit tests covering all three branches

### F-013: Embedded torrent engine

`kino-torrent` crate wraps librqbit.

librqbit session config (locked):

- Total cache size: configurable, default 4 GiB on Linux, 2 GiB on Android
- DHT enabled
- PEX enabled
- LSD enabled
- Supplementary trackers list shipped with the app (§8)
- Port: OS-assigned
- Max connections per torrent: 200
- Max upload speed: unlimited (user can throttle in settings)
- Max download speed: unlimited

Streaming output via `kino-server` (axum):

- Bind `127.0.0.1:0`, OS-assigned port saved in shared state
- Route: `GET /stream/{token}` with Range support
- Token: UUID v4, valid until `stop_playback(token)` or app shutdown
- Response: `206 Partial Content` for ranges, `200 OK` for full; `Accept-Ranges`, `Content-Range`, `Content-Length`, `Content-Type` correctly set

Cache eviction (LRU with protection):

- Protected pieces:
  - Within ± 60s of any active playhead
  - First piece of each video file
  - Last piece of each video file (moov atom protection)
- Eviction order: oldest non-protected first
- If cache full and no evictable: serve from network without cache

**Code acceptance:**

- Adding a magnet returns a streaming URL within 5s if metadata is fetchable
- Range requests work; player seek does not break the scheduler
- Cache eviction does not break ongoing playback
- Cache directory relocatable via settings (with app restart)
- Integration test: feed a known torrent fixture, stream it, verify byte-for-byte over HTTP

### F-014: Adaptive buffer

Implemented in `kino-torrent` as a piece scheduler atop librqbit, using its piece priority/deadline API.

Per-stream state:

- `dl_rate_rolling`: rolling average download rate, window 30s, sampled every 1s
- `file_bitrate`: `(file_size_bytes × 8) / duration_s` (duration from metadata or torrent media probe)
- `position_s`: latest playhead from player events
- `pieces_ahead_seconds`: seconds of playback downloaded ahead of `position_s`

All thresholds defined in §8.

State machine (recomputed every 5s and on events):

```
remaining_bytes        = file_size - bytes_downloaded
time_to_dl_remaining   = remaining_bytes / dl_rate_rolling (∞ if dl_rate ≈ 0)
time_to_play_remaining = duration_s - position_s

if time_to_dl_remaining <= time_to_play_remaining - safety_margin_s:
    state = SAFE
else:
    deficit_s = time_to_dl_remaining - (time_to_play_remaining - safety_margin_s)
    required_prebuffer_s = max(prebuffer_target_s, deficit_s)
    state = NEEDS_PREBUFFER(required_prebuffer_s)

if pieces_ahead_seconds < safety_margin_s × 0.5:
    state = REBUFFER
```

Player coordination:

- On initial play and on seek: backend sets player to paused; emits `buffer:status` with `required_prebuffer_s` and progress; resumes player when satisfied
- During playback, on transition into REBUFFER: backend pauses player via Tauri command; emits `buffer:status`; resumes when ahead is restored
- Backend never resumes player without an explicit player command (player is the source of truth on play/pause state)

Piece priorities mapped to librqbit:

- Window `[position, position + 60s]`: HIGHEST
- Window `[position + 60s, position + 300s]`: HIGH
- Last piece of the active file: HIGH
- All others: NORMAL

UI overlay during prebuffer/rebuffer: shows "Buffering for smooth playback" + progress bar driven by `pieces_ahead_seconds / required_prebuffer_s`, current download rate, and ETA.

**Code acceptance:**

- Unit tests cover the state machine with mocked rate, position, file size, duration
- Integration test on a synthetic slow torrent: prebuffer engages, math is satisfied, playback proceeds without underrun
- Integration test on fast torrent: state stays SAFE, no overlay shown

**Human verification:**

- On real-world slow torrent on Shield, prebuffer overlay appears, playback starts when expected, completes without underrun

### F-015: Native player integration

#### Android / Android TV (locked architecture)

Implementation: native Kotlin `PlayerActivity` in `android/player-plugin/`, wrapped by a Tauri plugin invoked by the Rust backend.

`PlayerActivity` responsibilities:

- Fullscreen activity, no system bars (`SYSTEM_UI_FLAG_IMMERSIVE_STICKY`)
- Owns an `ExoPlayer` instance configured for streaming HTTP local
- Native UI controls (Kotlin Views): play/pause, seek bar, audio track selector, subtitle track selector, info panel
- Handles D-pad / gamepad / touch / keyboard events
- Communicates back to Tauri plugin via local IPC; plugin forwards as Tauri events: `player:position`, `player:state`, `player:error`, `player:exit`

ExoPlayer configuration:

- Decoders: hardware preferred via `MediaCodecSelector.DEFAULT`. For DV content (profile 5/8.1 detected in stream metadata), force selection of a DV-capable decoder.
- Dolby Vision: profiles 5 and 8.1 supported. Profile 7 explicitly out of v1 (best-effort, not blocking).
- HDR: HDR10 and HDR10+ passthrough when sink declares support via `Display.HdrCapabilities`.
- Audio: passthrough for TrueHD, DTS-HD MA, DTS-X, E-AC3 JOC (Atmos), AC3, EAC3, DTS via `AudioCapabilities`. Falls back to decode for unsupported sinks. Per-codec toggles in settings.
- Subtitle parsers (Media3 built-in):
  - **Tier 1 (required)**: SRT, WebVTT, SSA/ASS basic dialogue lines and positioning
  - **Tier 2 (best-effort, not blocking)**: PGS, ASS with advanced effects (karaoke, complex animations)
- Tunneling mode enabled on Android TV when `Util.getTunnelingV21SupportedMimeType()` returns supported

Activity lifecycle:

- Launch with stream URL + resume position: load, seek to resume_position, play
- Back press: pause, exit activity, emit `player:exit` with final position
- Pause (e.g., user opens recent apps): pause player, save position
- Resume: stay paused; user explicitly presses play to resume
- Error: emit `player:error`, show error overlay, allow back

#### Linux (locked architecture)

Implementation: libmpv via `libmpv-rs` rendered into a GL surface owned by the Tauri window.

- The SolidJS frontend reserves a fullscreen container for the GL surface during playback
- mpv renders directly into the surface
- Controls (play/pause, seek, audio/sub track selectors, info) are SolidJS overlay elements composited over the surface by the browser
- mpv events bridged to the frontend via Tauri events

mpv config shipped with the app (`crates/kino-server/assets/mpv.conf`):

```
profile=high-quality
hwdec=auto-safe
keep-open=yes
cache=yes
demuxer-max-bytes=200M
demuxer-readahead-secs=20
audio-spdif=ac3,dts,eac3,truehd,dts-hd
sub-auto=fuzzy
sub-ass=yes
```

**Code acceptance:**

- Android: `PlayerActivity` plays a test stream (HTTP local) end-to-end, emits position events every 5s, exits cleanly with final position
- Android: SRT subtitle test renders correctly
- Android: SSA/ASS basic subtitle test renders correctly (dialogue lines visible, positioned)
- Linux: libmpv plays the same test stream end-to-end with controls overlay functional
- Both: seek works without breaking the adaptive buffer scheduler
- Both: player exit always triggers final position save

**Human verification:**

- DV Profile 5 .mkv on Shield Pro 2019: TV DV indicator lights, video plays
- Atmos TrueHD track on AVR-connected Shield: AVR shows Atmos / TrueHD
- ASS-subbed anime: subtitles render with original styling (tier 1 features at minimum)
- Linux: equivalent content plays correctly via libmpv

### F-016: Settings screen

Sections (locked order):

1. **API keys**
   - TMDB (required), Trakt, TVDB, Fanart.tv
   - Each: paste field + Test button
   - Inline link to provider key registration

2. **Addons**
   - Installed list: name, version, types served, enable toggle, uninstall (except Cinemeta)
   - Add by URL button
   - Recommended addons list with one-tap install (§8)
   - Drag-to-reorder for display order on home

3. **Language**
   - Primary metadata language (locale dropdown)
   - Fallback chain (up to 3 ordered)
   - UI language (English, French; defaults to system if matched)

4. **Cache**
   - Path (with directory picker)
   - Size limit (slider, 1 GiB to 100 GiB on Linux, 1 GiB to 50 GiB on Android)
   - Current usage display
   - Clear cache button (confirmation modal)

5. **Buffer**
   - Safety margin in seconds (default 30)
   - Initial prebuffer target in seconds (default 15)
   - Advanced toggle reveals: piece priority windows, recompute interval

6. **Player (Android only)**
   - Audio passthrough per codec: TrueHD, DTS-HD, DTS-X, Atmos / E-AC3 JOC, AC3, DTS, EAC3 (toggles)
   - Force hardware decoder toggle (default on)
   - Tunneling mode toggle (default on, Android TV only)

7. **Display**
   - Tile size (small / medium / large)
   - Focus animation toggle
   - Show NSFW content toggle (default off; passed to addons that support filtering)
   - Input profile override (auto / touch / dpad / kbm)
   - High-contrast theme toggle

8. **About**
   - Version (`Cargo.toml` workspace version)
   - Commit SHA (build-time injected)
   - Export logs button: zips logs folder to a chosen location
   - License: MIT, full text accessible
   - Link to GitHub repo

No external player toggle. No Real-Debrid integration. No voice search.

**Code acceptance:**

- All settings persist across restarts
- Test buttons return clear success/failure with error reason
- Reset to defaults button with confirmation restores out-of-box state
- All settings navigable end-to-end with D-pad only

### F-017: Input handling

Runtime input profile detection. App auto-selects on launch and adapts when devices appear/disappear. User can force a profile in settings.

#### Android TV — primary: D-pad + gamepad

| Action | D-pad | Gamepad | Keyboard / mouse |
|---|---|---|---|
| Navigate focus | Directions | D-pad + Left stick | Arrows, mouse |
| Activate | Center / Enter | A | Enter, click |
| Back | Back | B | Esc, right-click |
| Context menu | Menu | Y | F10, right-click |
| Search | (mic if present) | Y on home | `/` |
| Play/Pause (player) | Play/Pause button | A or Start | Space |

#### Android (mobile) — primary: touch

| Action | Touch | Gamepad | KBM (optional) |
|---|---|---|---|
| Navigate focus | Tap to focus | D-pad + Left stick | Arrows, mouse |
| Activate | Tap | A | Enter, click |
| Scroll | Swipe | Left stick / D-pad | Scroll wheel, arrows |
| Context | Long-press | Y | Right-click, F10 |
| Back | System back gesture | B | Esc |

#### Linux — primary: KBM

| Action | KBM | Gamepad | Touch (optional) |
|---|---|---|---|
| Navigate | Arrows + mouse | D-pad + Left stick | Tap to focus |
| Activate | Enter / click | A | Tap |
| Context | Right-click / F10 | Y | Long-press |
| Search | `/` | Y on home | n/a |
| Back | Esc | B | Back gesture / button |
| Play/Pause (player) | Space | A or Start | Tap controls |

**Code acceptance:**

- Each profile is testable via mocked input events; UI responds correctly
- Plugging a gamepad mid-session causes focus visuals to adapt within 2s

**Human verification:**

- Non-tech user navigates Home → detail → play → back to Home on Shield with the remote only
- Linux user does the same with keyboard only

### F-018: Build, packaging, distribution

#### Stable signing keystore for Android (locked)

A keystore is committed at `android/keystore/kino-dev.keystore`, generated in Session 001 with exact parameters from F-001. Documentation in `README.md` and `android/keystore/README.md` explains:

- Keystore is intentionally committed for sideload reproducibility
- Updates to the APK reinstall over previous installs because the key is stable
- Not a security control; anyone with the repo can sign as kino
- For app store distribution (v2), generate a private keystore stored as GitHub secrets

CI uses this keystore directly. No GitHub secrets required to build releases in v1.

#### `.github/workflows/ci.yml` (locked)

Triggered on push to any branch and on pull request.

Jobs:

- `lint`:
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cd frontend && npm ci && npm run lint && npm run typecheck`
- `test`:
  - `cargo test`
  - `cd frontend && npm test -- --run`
- `build-linux`:
  - `cargo tauri build --target x86_64-unknown-linux-gnu`
  - Uploads AppImage as artifact
- `build-android`:
  - `cargo tauri android build --apk` (universal)
  - Signed with `kino-dev.keystore`
  - Uploads APK as artifact

All jobs must pass for PR mergeability. Cache strategy: cache `~/.cargo/registry`, `target/`, `frontend/node_modules/`.

#### `.github/workflows/release.yml` (locked)

Triggered on tag matching `v*`.

Jobs:

- `build-linux-x86_64`: AppImage, .deb (cargo-deb), .tar.gz
- `build-android-universal`: signed APK (universal)
- `build-android-arm64-v8a`: signed APK (per-ABI)
- `build-android-armeabi-v7a`: signed APK (per-ABI)
- `build-android-x86_64`: signed APK (per-ABI, emulator testing)
- `generate-sbom`: cargo-cyclonedx + syft on the universal APK
- `release`:
  - Create GitHub Release with auto-generated notes from commit log since previous tag
  - Attach all artifacts and SBOMs
  - Mark as prerelease for `v*-alpha.*`, `v*-beta.*`, `v*-rc.*`

Artifact naming (locked):

- `kino-${version}-linux-x86_64.AppImage`
- `kino-${version}-linux-x86_64.deb`
- `kino-${version}-linux-x86_64.tar.gz`
- `kino-${version}-android-universal.apk`
- `kino-${version}-android-arm64-v8a.apk`
- `kino-${version}-android-armeabi-v7a.apk`
- `kino-${version}-android-x86_64.apk`
- `kino-${version}-sbom-cyclonedx.json`
- `kino-${version}-sbom-syft.spdx.json`

Re-running release for the same tag: idempotent (skip if release exists) or fail fast.

Android build parameters (locked):

- `minSdk` 24, `targetSdk` 34, `compileSdk` 34
- Rust Android targets: aarch64-linux-android, armv7-linux-androideabi, i686-linux-android, x86_64-linux-android
- Leanback support: `<uses-feature android:name="android.software.leanback" android:required="false" />` in `AndroidManifest.xml`, plus `<category android:name="android.intent.category.LEANBACK_LAUNCHER" />` on the main activity

**Code acceptance:**

- CI passes on a clean main checkout
- Tag `v1.0.0-alpha.1` produces a GitHub Release with all 9 artifacts above
- Universal APK installs on a real Android phone (arm64)
- Universal APK installs on a real Android TV (Shield Pro 2019)
- AppImage runs on Ubuntu 22.04 and 24.04 (`./kino-*.AppImage` launches GUI)
- Reinstalling an updated APK over a previous version succeeds (same keystore)

## 5. Non-functional requirements

### Performance targets (locked)

- Cold start to home screen: < 3s on Shield Pro 2019, < 2s on Linux desktop with SSD, < 4s on mid-range Android phone (2020+)
- Tile focus response: < 100ms
- Catalog load (cache hit): < 200ms
- Catalog load (cache miss with availability check): < 5s perceived (progressive render)
- Search "first result visible": < 500ms after typing stops

### Reliability

- App does not crash on network loss; UI degrades to "offline" state with retry button
- Player recovers from torrent stall via the rebuffer flow without crashing
- DB writes atomic; partial writes on crash recoverable via SQLite WAL
- Panic hook installed in Rust; panics logged with backtrace before exit
- Frontend errors caught at root error boundary and logged

### Privacy

- No telemetry in v1
- Logs local-only, never auto-uploaded
- API keys stored plain text in SQLite (acceptable v1)
- No third-party SDKs that phone home

### Logging

- Library: `tracing` + `tracing-subscriber`
- Levels: INFO default, DEBUG when "advanced logging" toggle is on in settings
- Output: rolling file in app log dir, max 5 MiB per file, keep last 5 files
- No remote logging endpoint

### Accessibility

- All interactive elements reachable via D-pad
- Focus indicator always visible (high contrast)
- No hover-only behaviors
- Subtitle rendering respects user-preferred size and color
- High-contrast theme available (toggle in Display settings)

### Internationalization

- UI strings in `frontend/src/locales/<lang>.json`
- v1 ships with: English (`en`), French (`fr`)
- Locale auto-detected from system, overridable in settings
- Metadata language preferences independent of UI language

## 6. Acceptance: PRD honored

### §6A Code acceptance (Claude Code verifies)

The code side of the PRD is honored when ALL of the following hold:

1. Every `F-XXX` from F-001 through F-018 is marked `[x]` in `STATE.md`
2. Every code-acceptance criterion within each F-XXX is verifiably satisfied by code on `main`
3. CI is green on `main`
4. A tag `v1.0.0-alpha.1` exists on `main` and has produced a GitHub Release with all 9 artifacts from F-018
5. Integration tests for F-013 and F-014 pass on the CI runner

When all five conditions hold, Claude Code declares `PRD COMPLETE` per AGENT_PROMPT Step 13.

### §6B Hardware verification (human verifies post-merge)

After Claude Code declares §6A complete, the human verifies the following on real hardware. Each item is a binary pass/fail recorded in `STATE.md` under "§6B Verification":

1. Linux AppImage launches on Ubuntu 22.04 and Ubuntu 24.04; home screen visible within 5s after TMDB key entered
2. Universal APK installs on a real Android arm64 phone; home screen visible; a stream plays end-to-end
3. Universal APK installs on Shield Pro 2019; home screen visible; navigation works with the Shield remote only
4. A DV Profile 5 movie plays on Shield with the TV DV indicator lit
5. An Atmos TrueHD track plays on Shield with the AVR showing Atmos
6. Adaptive buffer engages on a real-world slow torrent: overlay appears, playback starts at the right time, completes without underrun
7. Continue Watching saves position correctly: stop at 5min, reopen, resume at 5min
8. Reinstalling an updated APK over a previous version succeeds

If any §6B item fails, the human files a regression in `STATE.md` under "§6B Regressions" with the failing item and observations. Claude Code's next session addresses it as the highest-priority scope.

§6B is a human checklist. Claude Code never marks §6B items as complete.

## 7. Architectural Decisions Record (locked)

Decisions are immutable except by human PRD revision.

| ID | Decision | Source |
|---|---|---|
| ADR-001 | License: MIT | Human directive |
| ADR-002 | Default branch: main | Human directive |
| ADR-003 | Sideload-only distribution in v1 | Human directive |
| ADR-004 | Stable signing keystore committed in repo for sideload reproducibility | F-018 |
| ADR-005 | Rust + Tauri 2 over Flutter / Kotlin Multiplatform | §3 |
| ADR-006 | librqbit over libtorrent C++ FFI | §3 |
| ADR-007 | Stremio addon protocol as sole source layer | §3 |
| ADR-008 | Local HTTP server (axum, 127.0.0.1) for player consumption | §3 |
| ADR-009 | SQLite via sqlx, WAL mode, pool size 4 | F-002 |
| ADR-010 | ExoPlayer (Media3) on Android via native PlayerActivity (Kotlin), not webview overlay | F-015 |
| ADR-011 | libmpv on Linux in-window with SolidJS overlay controls | F-015 |
| ADR-012 | SolidJS over React | §3 |
| ADR-013 | Single SolidJS bundle, runtime input profile detection | F-017 |
| ADR-014 | Cinemeta as sole pre-installed addon, non-removable | F-007 |
| ADR-015 | No external player intent delegation | §2 out-of-scope |
| ADR-016 | TailwindCSS for styling | §3 |
| ADR-017 | `tracing` for logging, `thiserror` + `anyhow` for errors | §3 |
| ADR-018 | `@solid-primitives/i18n` for frontend i18n | §3 |
| ADR-019 | Cargo workspace with 5 crates: kino-core, kino-torrent, kino-server, kino-addons, kino-metadata | §3 |
| ADR-020 | Android minSdk 24, targetSdk 34, compileSdk 34 | F-018 |
| ADR-021 | Subtitle tiers: tier 1 required, tier 2 best-effort | F-015 |
| ADR-022 | DV profile 7 out of v1 scope (best-effort) | F-015 |
| ADR-023 | Daily trending shuffle seeded by SHA256(date \|\| install_id) | F-004 |
| ADR-024 | Stream filename parsing via regex set defined in §8 | F-010 |
| ADR-025 | UTF-8 throughout; all paths handled as `PathBuf` | engineering hygiene |
| ADR-026 | Single workspace version; updated by the release session | F-018 |
| ADR-027 | Acceptance §6 split into §6A (CC) and §6B (human) | §6 |
| ADR-028 | Daily randomization PRNG: `rand_chacha::ChaCha20Rng` | F-004 |

## 8. Implementation reference data

### Numeric constants (locked)

Implemented in `crates/kino-core/src/constants.rs`:

| Constant | Value |
|---|---|
| `SAFETY_MARGIN_S` | 30.0 |
| `PREBUFFER_TARGET_S` | 15.0 |
| `PIECE_PRIORITY_HIGH_WINDOW_S` | 60.0 |
| `PIECE_PRIORITY_MED_WINDOW_S` | 300.0 |
| `DL_RATE_WINDOW_S` | 30.0 |
| `RECOMPUTE_INTERVAL_S` | 5.0 |
| `AHEAD_CHECK_INTERVAL_MS` | 250 |
| `CACHE_DEFAULT_LINUX_GIB` | 4 |
| `CACHE_DEFAULT_ANDROID_GIB` | 2 |
| `AVAILABILITY_CONCURRENCY` | 8 |
| `AVAILABILITY_TIMEOUT_S` | 5 |
| `STREAM_AVAILABILITY_TTL_S` | 1800 (30min) |
| `TRENDING_TTL_S` | 21600 (6h) |
| `META_TTL_S` | 86400 (24h) |
| `SEARCH_TTL_S` | 3600 |
| `ARTWORK_TTL_S` | 604800 (7d) |
| `HTTP_TIMEOUT_S` | 10 |
| `HTTP_RETRY_BACKOFF_S` | [1, 2, 4] |
| `SEARCH_DEBOUNCE_MS` | 300 |
| `SEARCH_PAGE_SIZE` | 20 |
| `RECENT_SEARCHES_MAX` | 10 |
| `CW_COMPLETION_THRESHOLD` | 0.95 |
| `CW_AUTOREMOVE_S` | 86400 (24h) |
| `PLAYER_POSITION_INTERVAL_S` | 5 |
| `TRENDING_RESULT_COUNT` | 50 |
| `TOP_TRENDING_QUARTILE` | 0.25 |
| `HIDDEN_GEMS_RATING_THRESHOLD` | 7.5 |
| `FINAL_LIST_PATTERN` | [T,T,T,G,G] repeating |
| `MAX_CONNECTIONS_PER_TORRENT` | 200 |

### Stream filename parsing regex set (locked)

Implemented in `crates/kino-addons/src/parse.rs`:

```
QUALITY (checked in this order):
  4K     -> (?i)\b(2160p|4K|UHD)\b
  1080p  -> (?i)\b1080p\b
  720p   -> (?i)\b720p\b
  SD     -> (?i)\b(480p|576p|DVDRip|SDTV)\b

HDR (checked in this order):
  DV     -> (?i)\b(DV|DoVi|Dolby[. ]Vision)\b
  HDR10+ -> (?i)\bHDR10\+
  HDR10  -> (?i)\bHDR10?\b

CODEC:
  AV1    -> (?i)\bAV1\b
  H265   -> (?i)\b(H[. ]?265|HEVC|x265)\b
  H264   -> (?i)\b(H[. ]?264|AVC|x264)\b

AUDIO (checked in this order):
  ATMOS    -> (?i)\bAtmos\b
  TRUEHD   -> (?i)\bTrueHD\b
  DTS_HD   -> (?i)\bDTS[-. ]?HD([. ]MA)?\b
  DTS_X    -> (?i)\bDTS[: -]?X\b
  EAC3     -> (?i)\b(EAC3|DDP|DD\+|E-AC-3)\b
  AC3      -> (?i)\b(AC3|DD)\b
  DTS      -> (?i)\bDTS\b
```

Required test fixtures (parsing tests must produce exactly these tags):

| Filename | Expected tags |
|---|---|
| `The Matrix 1999 2160p UHD BluRay HEVC TrueHD Atmos 7.1-FraMeSToR` | 4K, H265, TRUEHD, ATMOS |
| `Inception 2010 1080p BluRay DV HDR10 x265 DTS-HD MA 5.1` | 1080p, DV, HDR10, H265, DTS_HD |
| `Some Show S01E01 720p WEB-DL DDP5.1 H.264` | 720p, EAC3, H264 |
| `Old Movie DVDRip XviD` | SD |

### Default supplementary trackers (locked)

`crates/kino-torrent/src/trackers.rs`:

```
udp://tracker.opentrackr.org:1337/announce
udp://tracker.torrent.eu.org:451/announce
udp://open.tracker.cl:1337/announce
udp://tracker.openbittorrent.com:6969/announce
udp://opentracker.i2p.rocks:6969/announce
udp://exodus.desync.com:6969/announce
udp://explodie.org:6969/announce
udp://tracker.moeking.me:6969/announce
udp://tracker.bittor.pw:1337/announce
udp://retracker.lanta-net.ru:2710/announce
udp://open.demonii.com:1337/announce
udp://tracker.tiny-vps.com:6969/announce
udp://www.torrent.eu.org:451/announce
udp://tracker.dler.org:6969/announce
```

### Recommended addons (locked)

`crates/kino-addons/src/recommended.rs`:

| Name | URL | Description |
|---|---|---|
| Cinemeta | `https://v3-cinemeta.strem.io/manifest.json` | Official metadata catalogs (pre-installed) |
| Torrentio | `https://torrentio.strem.fun/manifest.json` | Torrent streams aggregator |
| OpenSubtitles v3 | `https://opensubtitles-v3.strem.io/manifest.json` | Community subtitles |
| Public Domain Movies | `https://public-domain-movies.now.sh/manifest.json` | Free public domain titles |

### Glossary

| Term | Definition |
|---|---|
| Addon | A Stremio-protocol-compliant HTTP service exposing catalogs, metadata, streams, subtitles |
| Source | An addon that produces stream entries (e.g., Torrentio) |
| Catalog | A paginated list of titles served by an addon |
| Stream | A playable URL or magnet returned by an addon for a given title |
| Safety margin | Seconds of playback that must remain after estimated download completion for playback to proceed without rebuffering |
| Prebuffer target | Seconds of playback downloaded ahead of the playhead before playback starts |
| 10-foot UI | Interface designed for viewing from couch distance (≈ 3m), large tiles, clear focus indicators, no small text |
| Input profile | Runtime-selected interaction mode (touch / D-pad / KBM) that toggles UI affordances |
| Install ID | UUID v4 generated on first launch, used to deterministically seed daily trending shuffle |
| §6A | Code-side acceptance, verified by Claude Code |
| §6B | Hardware-side acceptance, verified by the human user post-merge |
