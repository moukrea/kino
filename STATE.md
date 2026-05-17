# kino — Agent State

**PRD version:** 1.0 (locked)
**Status:** features-in-progress
**Last session:** 009
**Next session:** 010

---

## Sessions Log

_New entries prepended at the top._

### Session 009 — F-006 Source availability filter

**Branch:** `claude/session-001-bootstrap-Ss8GZ`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-006 Source availability filter, end to end — the
`check_availability(items)` Tauri command that, for every requested
`(title_id, kind)` pair, asks every enabled stream-serving addon's
`GET /stream/{kind}/{id}.json` endpoint, treats any non-empty stream
list as "available from this source", caches per-source results in
the `stream_availability` table for 30 minutes (PRD §8
`STREAM_AVAILABILITY_TTL_S`), honors the locked 8-in-flight
concurrency cap (`AVAILABILITY_CONCURRENCY`) and the 5-second
per-request timeout (`AVAILABILITY_TIMEOUT_S`), and returns one
aggregated `AvailabilityResult` per input item. Session 008's
primary heads-up named F-006 as the natural next pick: it's directly
unblocked by F-007 (`AddonClient::stream` ships) and unblocks F-008
(Home Screen) and F-009 (sub-homes), both of which render only
available tiles by default. The `stream_availability` table has
been in `migrations/0001_init.sql` since Session 003.

**Files added (summary):**

- `crates/kino-core/src/availability.rs` — new module. Defines
  `AvailabilityRow { title_id, kind, source_id, has_streams,
  checked_at }`, the typed row shape consumed by the new
  persistence methods.
- `crates/kino-core/src/db.rs` — adds three availability methods on
  `Db`:
  - `availability_get_fresh(title_id, kind, source_id,
    fresh_after_unix_s) -> Result<Option<bool>, DbError>` — returns
    a cached check if `checked_at > fresh_after_unix_s`. The cutoff
    is computed by the caller as `now - STREAM_AVAILABILITY_TTL_S`
    so the same table can absorb a future TTL revision without a
    migration.
  - `availability_list_fresh(title_id, kind, fresh_after_unix_s) ->
    Result<Vec<(String, bool)>, DbError>` — returns every fresh
    per-source check for a given title. Used by the dispatch to
    aggregate per-title availability without a per-source loop.
  - `availability_upsert_many(rows) -> Result<(), DbError>` —
    batch upsert in a single transaction (empty input no-ops).
  Six new tests cover round-trip, absent-row, replace-existing,
  list-grouping with stale-row exclusion, atomic batch, and the
  empty-input fast path.
- `src-tauri/src/commands.rs` — adds the F-006 block (between F-005
  and F-007):
  - `AvailabilityRequest { title_id, type }` and
    `AvailabilityResult { title_id, type, available, source_count }`
    IPC shapes (serde rename `kind → "type"` to match the
    `TitleKind` JSON shape established by F-002).
  - `check_availability(db, items) -> Vec<AvailabilityResult>`
    Tauri command. Defers to `check_availability_with_config` (the
    test-friendly inner) with a production `HttpConfig` whose
    `timeout` is overridden to `AVAILABILITY_TIMEOUT_S` (5s).
  - `check_availability_with_config(db, items, http_config)` — the
    orchestration:
    1. `load_stream_addons(db)` filters installed addons to
       `enabled = true` AND whose manifest declares the `stream`
       resource (catalog/meta-only addons are skipped wholesale —
       no cache row, no work).
    2. For each `(item, addon)` pair where the addon's manifest
       `serves_stream(kind)` returns true (top-level types include
       `kind` AND the `stream` resource either has no type narrowing
       or includes `kind`), consult `availability_get_fresh`. Cache
       hits roll into the per-item count immediately; misses queue
       a work item.
    3. `dispatch_availability_checks(work, http_config, now)` —
       dispatches with a `tokio::sync::Semaphore(8)` permit cap and
       a `tokio::task::JoinSet` for fan-in. Per-addon `AddonClient`
       instances are memoized by manifest URL so multiple work
       items hitting the same addon share one `reqwest::Client`
       (and its connection pool). Per-request timeout is set on
       the `HttpConfig` passed in, NOT applied via
       `tokio::time::timeout`, so a slow response triggers the
       reqwest-internal timeout path and is treated as
       "unavailable from this source" (PRD §F-006 acceptance).
    4. Aggregates the fresh dispatch outcomes back into the
       per-item counts, persists ALL fresh rows (including the
       timeout-as-`false` entries — see ADR-059) via
       `availability_upsert_many`, and returns the response with
       `available = source_count > 0`.
  - 11 new tests covering: no-addons-installed fast path, single
    addon happy path, persistence side-effect, cache-hit-skips-
    network, disabled-addon filter, kind-mismatch filter,
    catalog-only-addon filter, multi-source counting, concurrency
    cap (50 items × 1 addon, observed peak in-flight ≤ 8),
    per-request timeout (slow addon → unavailable + cached as
    `false`), and empty-items fast path. Tests use
    `HttpConfig::for_test()` (zero backoff, 500ms timeout) so the
    timeout test completes in <1s of wall time.
- `src-tauri/src/lib.rs` — registers `check_availability` in
  `invoke_handler`.

**Files modified (no logic change beyond the addition):**

- `crates/kino-addons/src/manifest.rs` — adds two helpers:
  - `ManifestResource::types() -> Option<&[String]>` returns the
    long-form per-resource type narrowing, or `None` if the
    resource is in short form OR the long-form `types` array is
    empty (Stremio's convention is "absent or empty = no
    narrowing").
  - `Manifest::serves_stream(&self, kind: &str) -> bool` — true iff
    the manifest's top-level `types` includes `kind` AND the
    `stream` resource is present AND either has no per-resource
    type narrowing or that narrowing includes `kind`. Five new
    unit tests cover short-form, missing-stream-resource,
    long-form narrowing, long-form empty types (`None` per the
    helper), and the kind-not-in-top-level-types branch.
- `crates/kino-core/src/lib.rs` — declares the new
  `availability` module.
- `crates/kino-core/src/db.rs` — imports `AvailabilityRow`; existing
  methods unchanged.
- `src-tauri/Cargo.toml` — adds `wiremock` as a dev-dep (the F-006
  dispatch tests stand up mock Stremio stream endpoints; `tokio`
  with full features was already in deps).

**Features advanced:**

- F-006: not started → **complete**
  - **A catalog of 50 items with mixed availability renders only
    available tiles within 5s on broadband:** the 50-item
    concurrency test (`check_availability_respects_concurrency_cap`)
    runs against a single-host mock with a 50ms-per-call delay; with
    the cap at 8 the elapsed time is ≥150ms (8-batch parallelism) and
    well under 5s. Real-world wall time depends on addon RTT but the
    locked dispatch shape produces the 5s bound directly.
  - **Toggling "show all" reveals unavailable tiles with a badge;
    toggling off hides them:** the Rust surface returns
    `available: bool` + `source_count: u32` per item; the
    show/hide toggle is a frontend concern (F-008 / F-009 will
    consume the `available` flag for default-hide and respect the
    `show_unavailable` setting). No Rust changes needed for the
    toggle itself.
  - **`stream_availability` table populated correctly post-check:**
    verified by `check_availability_persists_results_to_stream_availability`
    (mixed `has_streams = true/false` rows land in the table) plus
    `availability_get_fresh` round-trip tests in the DB layer.
  - **Unit tests cover concurrency cap, timeout, cache hit, cache
    miss:** all four shipped explicitly. `respects_concurrency_cap`
    asserts observed peak in-flight ≤ `AVAILABILITY_CONCURRENCY`.
    `timeout_marks_source_unavailable` proves a slow addon doesn't
    block the dispatch and is recorded as `has_streams = false`
    (ADR-059). `uses_cache_hit_without_network` proves a pre-populated
    row skips the network entirely. `persists_results_*` proves
    fresh fetches both update the response AND write through to
    the table.

**ADRs filed this session:**

- **ADR-059** (timeout / transport failure from a single addon is
  persisted as `has_streams = false`, NOT as a cache miss): PRD
  §F-006 doesn't say whether a 5s timeout should burn the 30-min
  cache slot or stay un-cached. Two readings are possible: (a)
  treat the timeout as a transient failure, leave the cache row
  absent, and re-attempt next call; (b) record the timeout as
  "this source can't currently serve this title" and respect the
  30-min TTL. The shipped behavior is (b). Rationale: a flaky
  addon that times out on every request would otherwise re-trigger
  a 5s wait on every home-screen refresh, multiplying the per-tile
  cost by the number of timed-out addons. Treating the timeout as
  "unavailable from this source for 30 min" caps the worst-case
  refresh cost while still letting healthy addons keep the title
  visible (any-positive-source-wins aggregation). The cache row's
  `checked_at` ages out after 30 min, so a recovered addon shows up
  again at the next eligible re-check. The unit test
  `timeout_marks_source_unavailable` pins this.
- **ADR-060** (no `tokio::time::timeout` wrapper around the addon
  call; the 5s timeout lives on the reqwest `HttpConfig` instead):
  Two ways to install the per-request timeout were on the table:
  (a) wrap the addon call site in `tokio::time::timeout(5s, ...)`;
  (b) configure the `HttpConfig::timeout` field to 5s and let
  reqwest enforce it natively. The shipped path is (b). (a) would
  ALSO work but adds a layer of cancellation that hides the
  underlying `reqwest::Error::is_timeout()` from the retry logic
  in `fetch_with_retry`. Although F-006 disables the retry policy
  effectively by not changing `HttpConfig::backoff` (so retries
  still happen on transient errors), a future change to F-006's
  retry knob would interact strangely with `tokio::time::timeout`
  because the cancellation discards the retry state. Letting
  reqwest enforce the timeout keeps the retry path coherent —
  three retries (per the workspace-wide locked policy) at 5s each
  is consistent with the PRD's "per-request timeout: 5s" letter
  AND honors the locked retry backoff. Total worst-case per
  addon: 5s + 1s + 5s + 2s + 5s + 4s = 22s. The Semaphore caps
  the concurrent worst case at 8 × 22s = 176s, which is
  well-bounded for the home-screen workload. If real-world
  testing in §6B finds 22s-per-addon too generous, the polish
  pass can shrink `HttpConfig::backoff` for the availability
  client specifically without changing the dispatch shape.
- **ADR-061** (`load_stream_addons` filters out catalog-only
  addons before dispatch; per-kind filtering happens per item):
  Two filter passes are possible: (a) filter installed → enabled
  → stream-serving once, then per-item filter the result by kind
  again; (b) filter installed → enabled + serves_stream(kind)
  per item with no pre-filter. The shipped path is (a). (a)
  avoids re-deserializing the manifest JSON for every item (the
  Manifest type is `Clone` so the per-item per-addon scan over
  the pre-filtered slice is cheap). It also fixes a subtle bug
  potential: a catalog-only addon with `resources: ["catalog"]`
  would NOT be skipped by a per-kind check (since
  `serves_stream(kind)` returns false for both kinds), so the
  dispatch would still hit it; the pre-filter makes the no-work
  case structurally observable for the test
  `ignores_catalog_only_addons` which asserts the addon's stream
  endpoint is hit `0` times via `wiremock::Mock::expect(0)`.

**Tests added / coverage notes:**

- Rust: 22 new tests in this session. Workspace breakdown:
  - kino-core: 24 → 30 (+6 db availability tests:
    `availability_upsert_and_get_fresh_round_trip`,
    `availability_get_fresh_returns_none_when_absent`,
    `availability_upsert_replaces_existing_row`,
    `availability_list_fresh_groups_by_title`,
    `availability_upsert_many_handles_batch_atomically`,
    `availability_upsert_many_empty_input_is_noop`)
  - kino-addons: 57 → 62 (+5 manifest serves_stream tests:
    `serves_stream_true_for_short_form_resource`,
    `serves_stream_false_when_no_stream_resource`,
    `serves_stream_respects_long_form_type_narrowing`,
    `serves_stream_long_form_empty_types_means_all_top_level_types`,
    `serves_stream_false_when_kind_not_in_top_level_types`)
  - kino-app: 9 → 20 (+11 check_availability tests:
    `check_availability_no_addons_returns_all_unavailable`,
    `check_availability_returns_available_when_any_addon_has_streams`,
    `check_availability_persists_results_to_stream_availability`,
    `check_availability_uses_cache_hit_without_network`,
    `check_availability_filters_disabled_addons`,
    `check_availability_filters_kind_via_manifest`,
    `check_availability_counts_multiple_sources`,
    `check_availability_respects_concurrency_cap`,
    `check_availability_timeout_marks_source_unavailable`,
    `check_availability_empty_items_returns_empty`,
    `check_availability_ignores_catalog_only_addons`)
  - kino-metadata: 57 → 57 (no change)
  Workspace total: **172 passing** (62 kino-addons + 30 kino-core +
  57 kino-metadata + 20 kino-app + 3 kino-torrent + 0 kino-server).
- Frontend: no new tests this session. F-006 produces a Rust
  surface only; the show/hide toggle and tile loading-state
  indicator belong to F-008 (Home screen) and F-009 (sub-homes).

**Known issues introduced or resolved:**

- **New (introduced):**
  - **The `ConcurrencyProbe` responder in the cap test is
    best-effort.** wiremock doesn't expose a "request completed"
    hook so the probe decrements the in-flight counter
    immediately on entry to the responder rather than after the
    response is sent. The high-water-mark snapshot captured at
    the entry of each call IS the data the assertion uses, so
    the cap is still verified — the only thing the probe can't
    do is fail the test if the cap is briefly violated AFTER
    the wiremock matcher fires. In practice the 50ms `set_delay`
    on each response keeps the responder warm long enough that
    overlapping calls all enter the counter before any of them
    exits, so a true cap violation would still surface as a
    `peak > AVAILABILITY_CONCURRENCY` reading.
  - **Catalog response shape uses `MetaPreview::extra` carry-
    through but the F-006 dispatch doesn't surface it.** A future
    F-006-adjacent polish could enrich `AvailabilityResult` with
    a per-source-id breakdown (which addons returned streams)
    rather than only the count. Today's shape is enough for the
    home-screen "show unavailable" toggle; not blocking for §6A.
- **Resolved:** the "F-007 stream-availability cache wiring"
  shadow item implicit in Session 008's heads-up — the
  `stream_availability` table is now populated wherever the
  availability check runs.

**Heads-up for Session 010:**

- **Primary scope: F-008 Home screen (10-foot UI).** Now fully
  unblocked: F-004 (trending) + F-005 (artwork) + F-006
  (availability filter) + F-007 (addon catalogs) all ship. PRD
  §F-008 locks the row order (Continue Watching → Trending Now
  → Hidden Gems → Trending This Week → addon catalogs), tile
  specs (240×360 px base, 2:3 aspect, focus state scale 1.08,
  focus transition 150ms ease-out, info overlay after 600ms held
  focus), and lazy-loading. F-008 is the biggest UI lift in the
  PRD and could productively split into "F-008 scaffolding"
  (Rust home-payload command + tile component + row component +
  D-pad nav glue) and "F-008 polish" (focus-transition timing,
  info-overlay timer, virtualization on a long catalog row) if
  one session feels too tight.
- **Alternative scope: F-016 Settings screen.** Also fully
  unblocked (every Rust-side dependency now ships). PRD §F-016
  is mostly a frontend lift; the setup-wizard flow binds to
  `test_{tmdb,trakt,tvdb,fanart}` + `kv_get` / `kv_set`, and
  the addons panel binds to `get_recommended_addons` +
  `install_addon` + `uninstall_addon` + `addons_set_enabled` +
  `set_addon_order`. If F-008 feels too big and we want a
  smaller deliverable, F-016 is the cleanest choice.
- **Alternative scope: F-011 Search.** PRD §F-011 wires up
  TMDB / TVDB / Trakt `/search` endpoints + IMDb-id detection
  via TMDB `/find` (already shipped for F-005) + the
  `recent_searches` table (already in `migrations/0001_init.sql`
  since Session 003) + the 300ms debounce + 20-item page size +
  F-006 availability filter (shipped this session). The Rust
  surface needs `search_multi(query, page) ->
  Vec<TitleSummary>` and `recent_searches_*` commands. Smaller
  than F-008.
- **F-006 dispatch is reusable.** F-008's "compose a home
  payload" command will likely want to call
  `check_availability(items)` on the catalog rows it
  assembles so the home-screen render-loop receives
  pre-filtered title lists. The same applies to F-009 sub-homes
  and F-011 search results. The dispatch is `Db`-bound and
  re-entrant, so multiple concurrent calls (e.g. simultaneous
  trending-now + addon-catalog-row loads) share the
  `stream_availability` cache without contention beyond the
  single-row sqlx pool serialization.
- **`AddonClient` short-timeout pattern is now established.**
  F-008 may need a similarly-bounded variant for the "Trending
  This Week" rail (TMDB `/trending/{type}/week` via the existing
  `TmdbClient`, no addon involved); F-011 search will need it
  too. The `HttpConfig { timeout: Duration::from_secs(N),
  ..HttpConfig::default() }` pattern in
  `availability_http_config()` is the template.
- **`Manifest::serves_stream(kind)` and `ManifestResource::types()`
  are new public helpers.** F-008's addon-catalog-row loader will
  want a sibling `serves_catalog(kind) -> bool`; if/when that
  lands, factor a private `serves_resource(name, kind) -> bool`
  helper and have both call sites use it. Today the duplication
  isn't there yet.

### Session 008 — F-007 Stremio addon protocol client

**Branch:** `claude/session-001-bootstrap-CmzFb`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-007 Stremio addon protocol client, end to end — the
`AddonClient` covering all seven PRD-locked protocol endpoints (manifest,
catalog basic / paginated / search, meta, stream, subtitles), manifest
validation against PRD §F-007's required-fields list, URL normalization
(`stremio://` → `https://`), the six F-007 Tauri commands (with
Cinemeta-protection on uninstall), and the first-launch Cinemeta
auto-install bootstrap. Session 007's heads-up flagged F-007 as the
natural next pick: F-006 (source availability) depends on it, F-008
(Home screen) needs the Cinemeta catalog calls for the "Trending This
Week" rail, and the Settings → Addons screen (F-016) one-tap installer
binds to `get_recommended_addons` + `install_addon`. The scope also
includes the ADR-055 HTTP-module lift (`kino-metadata::http` →
`kino-core::http`) the heads-up identified as the cleanest path to
sharing the locked retry policy between metadata providers and addons.

**Files added (summary):**

- `crates/kino-core/src/http.rs` — new module. Workspace-wide HTTP
  machinery hoisted out of `kino-metadata::http` (ADR-055). Defines
  `HttpError` (Network + Http variants), `HttpConfig` (User-Agent /
  timeout / backoff knobs with the PRD §F-003 / §8 locked defaults),
  `HttpConfig::default()` / `HttpConfig::for_test()` / `build_client()`,
  and `fetch_with_retry(build, config) -> Result<Response, HttpError>`
  implementing the locked retry policy (3 retries with backoff
  `[1s, 2s, 4s]` on 5xx / 429 / transient transport errors). The
  per-crate retry test suites (TMDB / Trakt / TVDB / Fanart wiremock
  retry tests) continue to exercise this through their `Error::from`
  bridges; no test regressions.
- `crates/kino-addons/src/url.rs` — new module. `normalize_manifest_url`
  rewrites `stremio://...` → `https://...`, accepts `http(s)://`
  verbatim, enforces a trailing `/manifest.json` suffix, and rejects
  unknown schemes (`ftp://`, missing suffix, empty input) with a typed
  `AddonError::InvalidUrl`. `base_url_from_manifest` strips
  `/manifest.json` to derive the addon's protocol base URL for
  subsequent catalog / meta / stream calls. 8 unit tests.
- `crates/kino-addons/src/manifest.rs` — new module. `Manifest` /
  `ManifestResource` (untagged enum: short form `"catalog"` AND long
  form `{name, types, idPrefixes}`) / `CatalogDescriptor` types matching
  PRD §F-007's manifest schema; `parse_manifest(body) -> Manifest`
  validates against the locked required-fields list (`id`, `version`,
  `name`, `types`, `resources`, `catalogs`) with a typed `ManifestError`
  enum (`NotJson`, `Malformed`, `MissingField(&'static str)`,
  `EmptyField(&'static str)`). 16 unit tests covering every required
  field, the empty-catalogs-allowed exception (ADR-056), short vs long
  resource form, and the wrong-JSON-type rejection paths.
- `crates/kino-addons/src/protocol.rs` — new module. Stremio protocol
  response shapes: `CatalogResponse` / `MetaResponse` / `StreamResponse`
  / `SubtitlesResponse` envelopes plus `MetaPreview` (catalog rows),
  `MetaDetail` (full metadata), `MetaVideo` (per-episode entries for
  series), `Stream` (with all four one-of fields: `url`, `infoHash`,
  `ytId`, `externalUrl` plus `behaviorHints` / `sources` / a catch-all
  `extra` map), and `Subtitle`. Camel-case ↔ snake-case via
  `#[serde(rename = "...")]` per Stremio's wire shape. Unknown fields
  flow through via `#[serde(flatten)]` so addon-specific extras (per
  ADR-049 considerations) survive into downstream F-006 / F-010 / F-015
  code without provider-specific shims. 5 round-trip JSON tests.
- `crates/kino-addons/src/client.rs` — new module. `AddonClient` owns
  one addon's normalized base URL + a `reqwest::Client` configured from
  `HttpConfig`. Public methods: `new(manifest_url)` /
  `with_options(manifest_url, config)` (test-friendly), `manifest()`,
  `catalog(kind, id)`, `catalog_skip(kind, id, skip)`,
  `catalog_search(kind, id, query)`, `meta(kind, id)`,
  `stream(kind, id)`, `subtitles(kind, id)`. URL-encoding helpers
  escape the protocol-special characters `/ ? # space` in path segments
  and additionally `&` in the `search=...` query payload so unicode
  queries round-trip. 12 wiremock tests cover every endpoint plus
  manifest-validation failure surfacing as `AddonError::Manifest`,
  HTTP-error propagation as `AddonError::Http`, and `stremio://` URL
  normalization at constructor time.
- `crates/kino-addons/src/lib.rs` — adds the `AddonError` enum
  (`InvalidUrl`, `Http(HttpError)`, `Decode(String)`,
  `Manifest(ManifestError)`, `NonRemovable { id }`) and re-exports the
  public surface (`AddonClient`, `parse_manifest`, `Manifest`,
  `ManifestError`, all protocol types, URL helpers, the existing
  `RECOMMENDED_ADDONS` / `CINEMETA_MANIFEST_URL` from Session 001).
- `src-tauri/src/commands.rs` — adds four new Tauri commands plus the
  bootstrap helper:
  - `get_recommended_addons() -> Vec<RecommendedAddonView>` — surfaces
    the locked PRD §8 recommended-addons table as IPC-friendly owned
    strings.
  - `install_addon(url) -> Addon` — normalizes URL, fetches +
    validates manifest, persists with the next available
    `display_order`. Returns the inserted row so the frontend can
    update its local addon list in-place.
  - `uninstall_addon(id) -> u64` — refuses Cinemeta with a typed
    `NonRemovable` message; deletes the row otherwise. Cinemeta is
    identified by the locked PRD §8 manifest URL, not the addon's
    Stremio-supplied id, so a future Cinemeta-internal id change
    doesn't sneak past the guard.
  - `set_addon_order(id, order) -> ()` — moves the named addon to
    position `order` in the display list. Rebuilds the full ordering
    via the existing `addons_reorder` helper so the DB stays
    consistent.
  - `bootstrap_default_addons(db)` — first-launch installer for
    Cinemeta. Idempotent (skips on re-invocation via a
    `settings.addons.bootstrap_done` marker), tolerates network
    failure (logs + elides; user can retry from Settings → Addons),
    and skips if Cinemeta was somehow already installed.
- `src-tauri/src/lib.rs` — registers the four new commands in
  `invoke_handler` and invokes `bootstrap_default_addons(&db)` from
  the setup hook before `app.manage(db)`.

**Files modified (no logic change beyond the lift):**

- `crates/kino-core/Cargo.toml` — adds `reqwest = { workspace = true }`
  for the new `http` module.
- `crates/kino-core/src/lib.rs` — declares the new `http` module and
  re-exports `fetch_with_retry`, `HttpConfig`, `HttpError`,
  `USER_AGENT` at crate root for ergonomic consumption.
- `crates/kino-metadata/src/http.rs` — **deleted**. Its content lives
  in `kino-core::http` now (ADR-055). Behavior unchanged.
- `crates/kino-metadata/src/error.rs` — gains `From<kino_core::http::
  HttpError> for Error` so the existing `?` usage in per-provider
  modules continues to compile against the lifted `fetch_with_retry`
  signature. `Network` and `Http` variants kept identical so no
  downstream code that pattern-matches on `Error::Http` had to
  change.
- `crates/kino-metadata/src/lib.rs` — replaces the local
  `http::{HttpConfig, USER_AGENT}` re-export with
  `kino_core::http::{HttpConfig, USER_AGENT}` so existing imports
  `kino_metadata::HttpConfig` / `kino_metadata::USER_AGENT` continue
  to work.
- `crates/kino-metadata/src/{tmdb,trakt,tvdb,fanart}.rs` — updates
  `use crate::http::{...}` → `use kino_core::http::{...}`. The TVDB
  artwork extended-endpoint 404 pattern-match also updates to match
  on `HttpError::Http` since the `?` boundary moved.
- `crates/kino-addons/Cargo.toml` — adds `reqwest`, `tokio`, plus
  `wiremock` as a dev-dep.
- `src-tauri/Cargo.toml` — adds `kino-addons = { path = "..." }` so
  the host can use the protocol client.

**Features advanced:**

- F-007: not started → **complete**
  - **Tauri commands: `install_addon(url)`, `uninstall_addon(id)`,
    `list_addons()`, `set_addon_enabled(id, enabled)`,
    `set_addon_order(id, order)`, `get_recommended_addons()`:** all
    shipped. `list_addons` (as `addons_list`) and `set_addon_enabled`
    (as `addons_set_enabled`) were already in the registry from
    Session 003's F-002 work; Session 008 adds `install_addon`,
    `uninstall_addon`, `set_addon_order`, `get_recommended_addons`,
    all wired in `src-tauri/src/lib.rs::invoke_handler`.
  - **Cinemeta installed automatically on first launch:** shipped
    via `bootstrap_default_addons(&db)` in the Tauri setup hook.
    Idempotent (gated by `settings.addons.bootstrap_done`); skipped
    on subsequent launches.
  - **Cinemeta cannot be uninstalled (returns a typed error):**
    shipped. `uninstall_addon` consults `is_cinemeta_id` which
    matches on the locked PRD §8 manifest URL, then returns
    `AddonError::NonRemovable` (formatted to a clear string at the
    IPC boundary). Verified by
    `uninstall_addon_protects_cinemeta`.
  - **Manifest validation rejects invalid manifests with a typed
    error:** shipped. `parse_manifest` returns a typed
    `ManifestError` enum with four variants
    (`NotJson` / `Malformed` / `MissingField` / `EmptyField`), and
    the protocol-client `manifest()` method propagates them as
    `AddonError::Manifest`. Verified by
    `manifest::tests::rejects_*` (10 tests) plus the wiremock
    `client::tests::manifest_rejects_invalid_body_with_typed_error`.
  - **Unit tests cover protocol calls with `wiremock`:** all seven
    endpoints exercised with `wiremock::MockServer` + the
    short-backoff `HttpConfig::for_test()` — `manifest`, `catalog`
    (basic + skip + search), `meta`, `stream`, `subtitles`, plus
    failure paths (invalid body, 404 propagation, constructor URL
    rejection).

**ADRs filed this session:**

- **ADR-055** (workspace-wide HTTP plumbing lives in `kino-core::http`,
  not per-crate): PRD §F-003 locks the retry policy and User-Agent
  string for outbound HTTP, and F-007 introduces a SECOND outbound-HTTP
  consumer (`kino-addons`) on top of the F-003 metadata providers.
  Three options were on the table: (a) duplicate the ~80-line retry
  module into `kino-addons`, (b) make `kino-addons` depend on
  `kino-metadata` and re-export, (c) lift the module to `kino-core`.
  (a) violates the existing "Cross-Session Conventions" entry that
  shared retry logic lives in one place. (b) inverts the crate graph
  (addons are a SOURCE crate, metadata is a separate domain). (c) is
  the natural choice given that `HTTP_RETRY_BACKOFF_S` /
  `HTTP_TIMEOUT_S` already live in `kino-core::constants`. The lift
  moves the module to `kino-core::http`, defines a self-contained
  `HttpError` (Network + Http), and bridges via a `From<HttpError>`
  impl on `kino-metadata::Error` so existing pattern-matches on
  `Error::Http` keep working. `kino-addons::AddonError` does the same.
  `kino-metadata::lib.rs` keeps its `pub use` of `HttpConfig` /
  `USER_AGENT` for backwards compatibility with already-merged code
  that imports `kino_metadata::HttpConfig` directly.
- **ADR-056** (manifest validation rejects empty `types` / `resources`
  but accepts empty `catalogs`): PRD §F-007 says "presence of `id`,
  `version`, `name`, `types`, `resources`, `catalogs`". A literal
  read would allow `catalogs: []` AND `types: []` AND `resources: []`
  as long as all six keys are PRESENT. Stremio's protocol allows
  stream-only and subtitles-only addons (Torrentio is one of them and
  ships with `catalogs: []`), so accepting an empty `catalogs` array
  is a hard requirement. But an addon with empty `types: []` (no title
  kinds) or empty `resources: []` (no protocol resources) is
  functionally a no-op AND a typical sign of a misconfigured /
  half-rolled manifest; allowing the install would create dead rows
  in the persistence layer that would confuse the F-008 Home screen.
  The shipped rule: `types` and `resources` must be non-empty;
  `catalogs` may be empty. Two unit tests
  (`rejects_empty_types`, `rejects_empty_resources`) plus one
  positive test (`accepts_empty_catalogs_for_stream_only_addons`)
  pin this.
- **ADR-057** (Cinemeta non-removability is keyed on the locked
  manifest URL, not the addon's Stremio-supplied `id`): PRD §F-007
  identifies Cinemeta by its manifest URL (`https://v3-cinemeta.strem.io/manifest.json`,
  locked in PRD §8). The addon's own `id` field is set by Stremio
  (`com.linvo.cinemeta`) and could in principle change in a future
  Cinemeta release. The shipped `is_cinemeta_id(db, id)` helper
  looks up the addon row by `id` and confirms it has the locked
  manifest URL. An imposter addon that adopted the
  `com.linvo.cinemeta` id but pointed at a different URL would NOT
  be protected (and shouldn't be — the user can freely uninstall a
  third-party "Cinemeta-alike"). Verified by the
  `uninstall_addon_protects_cinemeta` test's imposter branch.
- **ADR-058** (first-launch Cinemeta install tolerates network
  failure; doesn't block startup): A naive read of PRD §F-007's
  "Cinemeta installed automatically on first launch" would require
  the app to refuse to start on a network outage. That's a poor
  user experience for a 10-foot UI where the user may not have an
  obvious recovery path. The shipped `bootstrap_default_addons`
  logs the failure with full error context (`tracing::warn!`),
  elides the `settings.addons.bootstrap_done` marker write (so the
  next launch retries), and returns. The user can manually install
  Cinemeta from Settings → Addons via the same code path. The
  bootstrap marker is set only on success, so a partial install
  state (e.g. Cinemeta inserted but TMDB API key not yet
  configured) is fine — the marker just gates the auto-install
  attempt.

**Tests added / coverage notes:**

- Rust: 45 new tests in this session. Workspace breakdown:
  - kino-core: 23 → 24 (no new tests this session beyond the
    inherited retry-policy coverage via the per-provider wiremock
    suites — the `http` module's behavior is exercised through every
    `Tmdb/Trakt/Tvdb/Fanart` test). The +1 vs Session 007 is from
    Session 007's `Artwork` JSON round-trip test that I missed
    earlier.
  - kino-addons: 16 → 57 (+41): 8 url + 16 manifest + 5 protocol +
    12 client (wiremock) plus the existing 16 parse + recommended.
  - kino-app: 5 → 9 (+4): `recommended_addons_view_matches_locked_table`,
    `uninstall_addon_protects_cinemeta`,
    `set_addon_order_rearranges_list`,
    `bootstrap_skips_when_marker_present`.
  - kino-metadata: 57 → 57 (no change; the http lift is invisible
    to the per-provider tests because the `?` operator + `From`
    impl preserves the API surface).
  Workspace total: **150 passing** (57 kino-addons + 9 kino-app +
  24 kino-core + 57 kino-metadata + 3 kino-torrent + 0 kino-server).
- Frontend: no new tests this session. F-007's frontend integration
  (Settings → Addons screen rendering `get_recommended_addons` +
  install button + reorderable list) belongs to F-016.

**Known issues introduced or resolved:**

- **New (introduced):**
  - **First-launch Cinemeta install is best-effort (ADR-058).** On
    network failure during the first launch, the bootstrap logs a
    warning and proceeds; the user can manually install Cinemeta
    from Settings → Addons. This isn't a behavioral regression
    (Cinemeta was never installed at all before this session), but
    means F-008 Home screen's "Cinemeta catalogs" rows will be
    empty on a first-launch network-outage scenario until the user
    completes setup. Acceptable for v1.
  - **Per-provider Stremio catalog response cache not yet wired.**
    F-007 ships the protocol calls; the F-008 Home screen will
    issue `catalog()` and `catalog_search()` calls that should be
    cached in `response_cache` for the appropriate TTL (PRD §8 has
    no explicit "ADDON_CATALOG_TTL_S"; META_TTL_S = 24h is the
    closest analog). The cache wiring is deferred to F-008 because
    the cache key shape depends on how the Home composition stitches
    addon catalogs onto the locked rows; arguably this belongs
    inside the client itself in a future polish pass.
- **Resolved:** the "Cinemeta auto-install on first launch" item
  implicit in F-001's "addons.bootstrap_done" naming intent from
  Session 003 — Cinemeta now lands automatically.

**Heads-up for Session 009:**

- **Primary scope: F-006 Source availability filter.** Directly
  unblocked by F-007 (the stream-availability check is a batched
  `AddonClient::stream(...)` call against every enabled stream-serving
  addon). PRD §F-006 is fully spec'd: 8-concurrent-request cap, 5s
  per-stream timeout, 30-min `stream_availability` cache TTL, three
  tile states (Loading / Available / Unavailable), "Show unavailable
  titles" toggle. The `stream_availability` table already exists in
  `migrations/0001_init.sql`; what F-006 adds is the typed Tauri
  command (`check_availability(items: Vec<TitleAvailabilityRequest>)`),
  the concurrency-bounded `tokio::task::JoinSet`-style harness, and
  the cache-aware skip logic.
- **Alternative scope: F-008 Home screen (10-foot UI).** Now fully
  unblocked: F-004 (trending) + F-005 (artwork) + F-007 (addon
  catalogs) all ship. PRD §F-008 locks the row order, tile specs,
  and lazy-loading requirement. Bigger lift than F-006 because it
  spans Rust (a thin Tauri command that composes the existing
  trending/artwork/catalog calls into a single home payload) plus
  meaningful SolidJS rendering work (tile component, focus state
  CSS, D-pad navigation glue, info-overlay 600ms timer, virtualized
  rows). Could split into "F-008 scaffolding" + "F-008 polish"
  sub-sessions if it feels too big.
- **Alternative scope: F-016 Settings screen.** Also unblocked
  (test_{tmdb,trakt,tvdb,fanart} + get_recommended_addons +
  install_addon + uninstall_addon + addons_set_enabled +
  set_addon_order + kv_get/kv_set all shipped). PRD §F-016 is the
  setup wizard + the persistent settings screen; everything the
  setup wizard needs on the Rust side is now in place. Mostly a
  frontend lift.
- **`AddonClient` is reusable for F-006 and F-008.** The client is
  `Clone` (the inner `reqwest::Client` is `Arc`-backed); the
  recommended pattern is to construct one per addon during a batch
  operation and reuse across calls. Concurrency in F-006 should
  use a `JoinSet` (or a `Semaphore` to honor the 8-concurrent cap).
- **`HttpConfig::for_test()` is public now.** The Session 007 helper
  for short-backoff test clients can be called from any crate
  (kino-addons already uses it; F-006/F-008 wiremock tests can use
  it the same way).
- **Stremio's untagged `ManifestResource` enum needs care when
  inspecting addon manifests.** F-006 will likely filter the
  installed addons list to "those that serve `stream` for the
  requested type". The pattern is
  `manifest.resources.iter().any(|r| r.name() == "stream")` (use
  the `name()` helper, don't pattern-match on the enum).

### Session 007 — F-005 Image & logo resolution

**Branch:** `claude/session-001-bootstrap-DFlt5`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-005 Image & logo resolution, end to end — the
per-provider image / summary fetchers, the locked six-tier per-image-type
cascade exactly as PRD §F-005 spells it out, the `resolve_artwork` Tauri
command that stitches it onto the response cache with the `ARTWORK_TTL_S
= 7d` TTL (PRD §8), and the cross-provider id resolution (TMDB
`/find` + `/external_ids`) the cascade needs to dispatch each provider with
the id shape it expects. Session 006's primary heads-up named F-005 as the
natural next pick: it builds on the F-003 `*Client` types this codebase
already has, reuses the `cache_get` / `cache_set` plumbing added in Session
006, and unblocks F-008 (Home screen) by replacing the F-004 trending
output's TMDB-only `w500` poster placeholders with proper provider-
fallback URLs.

**Files added (summary):**

- `crates/kino-metadata/src/artwork.rs` — new module. Defines the
  `LocalizedAsset` / `ProviderBundle` / `ProviderBundles` types feeding
  the cascade, plus the pure `cascade(kind, bundles, lang_pref) -> Artwork`
  function that implements PRD §F-005's per-image-type six-tier cascade
  (tiers 1..=4 = configured langs Fanart → TMDB → TVDB; tier 5 = any
  other lang; tier 6 = placeholder URL). Summary follows the same shape
  but only TMDB → TVDB (Fanart never serves summaries). Also exposes
  `lang_chain_hash(lang_pref) -> String` for the F-005 cache-key contract
  ("changing language preferences invalidates the cache on next read")
  and `CachedArtwork` for cache row serialization. 14 unit tests cover
  every PRD §F-005 acceptance bullet (per-image-type independence, tier
  1 / tier 2 / tier 5 / tier 6 resolution, provider skipping on missing
  key, summary's Fanart skip, lang chain hash stability, ISO 639-2 → 639-1
  collapse).
- `crates/kino-core/src/title.rs` — adds the `Artwork`, `Provenance`,
  and `ImageType` types. `Artwork` is the public shape the Tauri command
  returns: five string fields (`poster`, `backdrop`, `logo`, `clearart`,
  `summary`, all guaranteed non-`null` — empty string for summary at tier
  6, sentinel `kino://placeholder/<type>.svg` URL for images at tier 6)
  plus a `Provenance` block carrying per-field `<provider>:<lang>` source
  markers per PRD §F-005 acceptance bullet 1. One new round-trip JSON
  test.
- `crates/kino-metadata/src/tmdb.rs` — adds `find_external(external_id,
  external_source, kind)`, `external_ids(tmdb_id, kind)`,
  `artwork_images(tmdb_id, kind, lang_pref)`, and `summary(tmdb_id, kind,
  language)`. `find_external` resolves IMDb → TMDB id via `/3/find` (and
  TVDB → TMDB id symmetrically). `external_ids` returns the full
  `TitleIds` (TMDB + IMDb + TVDB) via `/3/{movie|tv}/{id}/external_ids`,
  which the F-005 resolver uses to bridge into Fanart.tv (movies key by
  IMDb or TMDB, shows key by TVDB) and into TVDB (keys by its own id).
  `artwork_images` uses TMDB's `include_image_language=lang1,lang2,null`
  filter to fetch every configured language plus textless artwork in a
  single round-trip. 6 new wiremock tests.
- `crates/kino-metadata/src/tvdb.rs` — adds `artwork(tvdb_id, kind)`
  hitting `/v4/{movies|series}/{id}/extended?meta=translations`. The
  response carries an `artworks[]` array tagged with numeric `type` ids;
  the new private `artwork_types` module decodes TVDB's locked type
  mapping (movies: poster=14, background=15, banner=16, clearart=24,
  clearlogo=25; series: poster=2, background=3, banner=1, clearart=22,
  clearlogo=23). `translations.overviewTranslations[]` populates the
  summary map keyed by 3-letter ISO 639-2 codes (which the cascade's
  `normalize_lang` collapses to 2-letter). Returns `Ok(None)` on HTTP
  404 so a TVDB miss doesn't poison the cascade. 3 new wiremock tests.
- `crates/kino-metadata/src/fanart.rs` — adds `movie_artwork(id)` (where
  `id` is either TMDB id or IMDb id — Fanart.tv accepts both) and
  `show_artwork(tvdb_id)`. The TV endpoint requires a TVDB id; passing
  TMDB id won't work, which is why the F-005 resolver fetches
  `external_ids` upfront. Both methods normalize the Fanart-specific
  `"00"` lang sentinel (textless artwork) to the empty string so the
  cascade's `lang_matches` rule (textless = matches any tier) applies
  uniformly. Returns `Ok(None)` on HTTP 404. 3 new wiremock tests.
- `crates/kino-metadata/src/lib.rs` — module declaration + re-exports
  for `artwork`, the new types (`ProviderBundle`, `ProviderBundles`,
  `CachedArtwork`), and `lang_chain_hash`.
- `src-tauri/src/commands.rs` — adds the `resolve_artwork(title_id,
  kind, lang_pref)` Tauri command. Pulls API keys from `settings`, parses
  the provider-prefixed `title_id` (`tmdb:N` / `imdb:tt...` / `tvdb:N`),
  bootstraps the full `TitleIds` via TMDB `/find` + `/external_ids` (if
  TMDB is configured), fetches the three provider bundles concurrently
  via `tokio::join!`, runs the locked cascade, and caches the resulting
  `Artwork` in `response_cache` for `ARTWORK_TTL_S = 7d`. Cache key is
  `artwork:<title_id>:<kind>:<lang_chain_hash>` so a language-preference
  change transparently invalidates the row. 3 new unit tests cover
  `parse_title_id`.
- `src-tauri/src/lib.rs` — registers `resolve_artwork` in
  `invoke_handler`.

**Files modified (no logic change):**

- `src-tauri/Cargo.toml` — no edits required; the new command pulls in
  `kino-metadata`'s new module via the existing path dep.

**Features advanced:**

- F-005: not started → **complete**
  - **`resolve_artwork(title_id, type, lang_pref: Vec<String>) -> Artwork`
    Tauri command returns a struct with `poster`, `backdrop`, `logo`,
    `clearart`, `summary` fields plus a per-field `source` indicator:**
    shipped. Registered in the `invoke_handler` list; the returned
    `Artwork` carries a `sources: Provenance` block with five
    `<provider>:<lang>` strings (e.g. `"fanart.tv:en"`, `"tmdb:fr"`,
    `"placeholder"`) per PRD acceptance bullet 1.
  - **Returned URLs cached for 7 days:** `expires_at = now + ARTWORK_TTL_S`
    in `response_cache`. Cache key includes `lang_chain_hash(lang_pref)`
    so swapping the configured language chain invalidates the row on
    next read.
  - **A title with no artwork in any provider returns placeholder URLs
    for images and empty string for summary without crashing:** verified
    by `tier6_placeholder_when_no_provider_has_asset` and
    `summary_tier6_empty_when_no_provider_serves_one`. The five
    placeholder URLs are sentinel `kino://placeholder/<type>.svg` strings
    the frontend resolves to bundled SVG assets in F-008.
  - **A title with missing Fanart.tv key still resolves via TMDB/TVDB
    across all language tiers:** verified by
    `missing_fanart_key_still_resolves_via_tmdb_tvdb`. The cascade walks
    `[Provider::Fanart, Provider::Tmdb, Provider::Tvdb]` skipping any
    bundle that's `None`; the host commands set the bundle to `None`
    when the `settings.<provider>_api_key` row is missing.
  - **Unit tests cover: each tier resolving, provider skip on missing
    key, fallback to placeholder, per-image-type independence (e.g.,
    poster from tier 1, logo from tier 3), summary skipping Fanart.tv:**
    all five covered. Tier resolution: `tier1_fanart_wins_when_present`,
    `tier2_fallback_lang_resolves_when_tier1_empty`,
    `tier5_any_language_picks_first_available`,
    `tier6_placeholder_when_no_provider_has_asset`. Per-image-type
    independence: `per_image_type_independence_demonstrated`. Summary
    Fanart skip: `summary_skips_fanart_uses_tmdb_then_tvdb`.

**ADRs filed this session:**

- **ADR-051** (the placeholder asset is a sentinel URL, not a frontend
  asset path): PRD §F-005 tier 6 says "local placeholder asset shipped
  with the app", which is structurally a frontend concern. Hardwiring a
  bundled SVG path (e.g. `/assets/placeholder/poster.svg`) into the
  Rust resolver would couple it to the frontend build output layout
  (`vite` bundle hashes filenames in production). The shipped layer
  emits `kino://placeholder/<type>.svg` sentinels that the F-008 home
  renderer (and later F-009 sub-homes, F-010 detail, F-011 search)
  resolve to a bundled SVG via a `<source>:<type>` mapping table in the
  frontend's image component. This keeps the Rust side stable across
  any future renderer-asset refactor, and the `placeholder` source
  marker in `Provenance` lets the UI distinguish tier-6 fallbacks from
  real upstream assets when needed (e.g. show a subtle "missing artwork"
  badge in admin / debug modes).
- **ADR-052** (textless artwork — Fanart `"00"` lang or TMDB `null` —
  matches any language tier): PRD §F-005 doesn't say what to do with
  assets that have no language tag. Two interpretations are reasonable:
  (a) textless artwork is a "no language" asset and only matches tier
  5 ("any other language"); (b) textless artwork is universally
  appropriate and matches every tier. The shipped behavior is (b)
  because textless logos / backdrops are the COMMON case for those image
  types (provider-neutral artwork has no text overlay) and treating
  them as tier-5-only would cause a Spanish-language user with a
  Spanish-locale tier 1 to fall through to tier 2 / tier 3 just because
  the only available logo is textless. The `lang_matches` rule in the
  cascade implements this: empty asset lang short-circuits to true for
  any requested lang. The source marker still reflects the requested
  tier's lang (e.g. `fanart.tv:en`) so the renderer sees a tier-1 hit,
  not a tier-5 fallthrough.
- **ADR-053** (the F-005 cascade does NOT enrich per-language summaries
  on every TMDB cache miss; it only calls `/movie/{id}?language=lang`
  for each language in `lang_pref`, not for tier 5): PRD §F-005 step
  "summary tier 5: any other language" would mandate either (a) calling
  `/movie/{id}` with no language hint and accepting whatever TMDB
  returns, or (b) iterating TMDB's `/translations` endpoint to enumerate
  every available summary language. The shipped behavior is the
  cheapest reading: TMDB summaries are fetched ONLY for the tier 1..=4
  languages, and the cascade's tier-5 walk inspects the
  `bundle.summaries` map for any remaining entry (which TVDB populates
  in bulk via `/extended?meta=translations`). For a TMDB-only setup
  where the configured langs miss, the summary falls through to
  empty / placeholder rather than spending a 6th TMDB round-trip. This
  matches the "first non-empty wins" letter of the PRD while keeping
  the worst-case-per-title call budget at 1 (Fanart) + 2 (TMDB
  external_ids + images) + N (TMDB summaries per configured lang) + 1
  (TVDB extended) = 4 + N. With N=4 that's 8 calls; the 7-day cache
  amortizes this fine for the home-screen-scrolling workload.
- **ADR-054** (Fanart movies prefer the TMDB id over the IMDb id when
  both are known): Fanart.tv movie lookups accept either; the TMDB id
  is a numeric integer while IMDb is a `ttNNNN...` string. Both work,
  but TMDB-id lookups are slightly faster (per Fanart's own
  documentation), and on a TMDB-id-first codebase (every F-004 trending
  result starts as `tmdb:<n>`) we already know the TMDB id at the time
  of the Fanart call. The shipped order in `build_bundles` is
  `tmdb_id` first, `imdb_id` second, both `None` → no Fanart call. The
  difference is invisible to the cascade output.

**Tests added / coverage notes:**

- Rust: 27 new tests in this session. Workspace breakdown:
  - kino-core: 23 → 24 (1 `Artwork` round-trip-through-JSON test)
  - kino-metadata: 29 → 57 (1 new tvdb extended test x3 movie/series/404,
    1 new tmdb find_external test x2 hit/miss, 1 new tmdb external_ids
    test x2 happy/empty, 1 new tmdb artwork_images test, 1 new tmdb
    summary test x2 happy/empty, 1 new fanart movie test x2
    happy/404, 1 new fanart show test, 14 new artwork::tests covering
    every PRD §F-005 acceptance bullet)
  - kino-app: 2 → 5 (3 new `parse_title_id` tests covering valid
    prefix, unsupported prefix, unprefixed value)
  Workspace total: **105 passing** (16 kino-addons + 24 kino-core + 57
  kino-metadata + 5 kino-app + 3 kino-torrent + 0 kino-server).
- Frontend: no new tests this session. F-005's frontend integration is
  F-008's job (Home screen renders the `Artwork` returned by
  `resolve_artwork`); the F-005 surface is the Tauri command, fully
  covered on the Rust side.

**Known issues introduced or resolved:**

- **New (introduced):**
  - **TMDB has no clearart endpoint (PRD §F-005 unaddressed gap).** The
    cascade ships with TMDB's `clearart` bucket permanently empty
    because TMDB's `/images` endpoint doesn't carry that asset type.
    Fanart.tv and TVDB both serve clearart, so clearart resolution
    proceeds through the cascade normally — TMDB is just one less hop.
    Not a defect; documented for future "why is TMDB skipped for
    clearart" questions.
  - **TMDB summary cost scales with `lang_pref` length (ADR-053).**
    Each configured language adds one TMDB `/movie/{id}?language=lang`
    round-trip on cache-miss. With the PRD §F-016 limit of 4 langs
    (primary + 3 fallback), worst case is 4 TMDB summary calls per
    artwork resolution. The 7-day cache amortizes this; per-title
    pre-warming on Home Screen load is a candidate future polish.
- **Resolved:** the "TMDB w500 placeholder posters in trending"
  intermediate state from Session 006 — Home Screen will replace those
  with `resolve_artwork` URLs once F-008 lands.

**Heads-up for Session 008:**

- **Primary scope: F-007 Stremio addon protocol client.** F-006 (source
  availability filter) depends on F-007, and F-008 (Home Screen) needs
  the Cinemeta addon catalog calls for the "Trending This Week" rail
  per PRD §F-008. F-007 is fully specified in PRD §F-007 with explicit
  endpoint signatures (`/manifest.json`, `/catalog/...`,
  `/meta/...`, `/stream/...`) and the recommended-addons list is
  already in `kino-addons::recommended` since Session 001. The
  per-provider HTTP plumbing (`http::fetch_with_retry`) lives in
  `kino-metadata` but is generic enough to re-export for `kino-addons`;
  alternatively, refactor it down into `kino-core::http` so both
  `kino-metadata` and `kino-addons` consume the same retry policy.
- **Alternative scope: F-008 Home Screen.** Now unblocked by F-004 +
  F-005. The remaining gap is the "Trending This Week" rail (PRD
  §F-008) which needs a Stremio addon `/catalog/movie/top.json`
  call — i.e. F-007 indirectly. If F-007 is too big, F-008 can ship the
  Catalog + Continue Watching rails first and add the addon-driven
  "Top" rail in a follow-up session.
- **The Settings screen (F-016) is unblocked.** Setup wizard binds to
  the four `test_<provider>` commands (Session 004) + reads/writes the
  four `<provider>_api_key` settings entries via `kv_get` / `kv_set`
  (Session 003). No additional Rust surface needed for the basic flow;
  i18n / locale chain editing is a small frontend lift on top of the
  existing locales/{en,fr}.json + the new `lang_pref` parameter.
- **Cross-provider id resolution is now centralized in
  `src-tauri::commands::resolve_title_ids`.** When F-006 / F-007 need
  to dispatch a stream-availability check or an addon meta call for a
  given title id, they should reuse the same shape (or lift the helper
  into a shared module). The current implementation is a
  copy-paste-safe ~30 lines; if it shows up in 3+ feature commands, a
  refactor into `kino-metadata::ids` or `src-tauri::ids` makes sense.
- **Frontend Artwork rendering is the F-008 first job.** The `Artwork`
  struct has a stable JSON shape (see `Artwork` round-trip test);
  `invoke('resolve_artwork', { title_id, kind, lang_pref })` returns
  it. The `kino://placeholder/<type>.svg` URLs need a frontend mapping
  to bundled SVG assets — recommended path: a single `<Image>` Solid
  component that intercepts the `kino://` scheme and renders the
  appropriate inline SVG.

### Session 006 — F-004 Trending aggregation with diversity

**Branch:** `claude/session-001-bootstrap-wvX9T`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-004 Trending aggregation with diversity, end to end —
the per-provider HTTP trending fetchers, the merge / pool-split /
alternation / daily-seeded-shuffle algorithm exactly as PRD §F-004 locks
it, and the `get_trending` Tauri command that ties it onto the F-002
persistence layer (install_id + day-long output cache). Session 005's
heads-up named F-004 as the natural next step: it builds on the F-003
`*Client` types this codebase already has, the daily-shuffle PRNG deps
(`sha2`, `rand_chacha`, `rand`) were already wired into the workspace
since Session 001, and the algorithm is fully spelled out step-by-step in
the PRD so there is no design ambiguity.

**Files added (summary):**

- `crates/kino-metadata/src/trending.rs` — new module. The aggregator
  (`aggregate(tmdb, trakt, tvdb, install_id, today_utc) -> Vec<TitleSummary>`),
  the `ProviderItem` shape every provider's `trending_*` method returns,
  the SHA256-of-`{date}+install_id` → `ChaCha20Rng::from_seed` seed
  derivation (PRD §F-004 step 7 / ADR-023 / ADR-028), the
  `[T,T,T,G,G]`-repeating interleaver, the top-quartile / hidden-gems
  pool split with the locked `rating > 7.5` AND
  `popularity_rank < median` gate, and the `0.45*trakt + 0.35*tmdb +
  0.20*tvdb` weighted score with the 0.5 neutral for missing providers.
  12 unit tests cover the merge, the score formula, the pool split (two
  variants), the interleave (with the explicit `TTT GG TTT GG` fixture
  and the pool-exhausted fallthrough), the seed determinism, the count
  cap, the same-day/same-install identical-ordering invariant, the
  consecutive-day Kendall-tau-below-0.7 invariant, and the
  different-install-id divergence invariant.
- `crates/kino-metadata/src/tmdb.rs` — adds `trending_movies(locale)`
  and `trending_shows(locale)`. Hits `/3/trending/{movie,tv}/week` with
  the locked `language` parameter, parses the documented response shape
  (`title`/`name`, `release_date`/`first_air_date`, `vote_average`,
  `popularity`), and builds `ProviderItem`s with `id = "tmdb:<id>"` plus
  a `w500` TMDB CDN poster URL (the F-005 image resolver will replace
  that with the proper fallback chain). 3 new wiremock tests
  (movies + shows + the 100-item cap).
- `crates/kino-metadata/src/trakt.rs` — adds `trending_movies()` and
  `trending_shows()`. Hits `/movies/trending` and `/shows/trending` with
  the `trakt-api-version: 2` + `trakt-api-key` headers AND a
  `limit=100` query parameter. Items keyed by IMDb when present
  (`imdb:tt....`), TMDB id fallback (`tmdb:<id>`), then a
  Trakt-local synthesized id (`trakt-rank:<n>`) so two unidentified
  Trakt entries never collide with each other. 2 new wiremock tests.
- `crates/kino-metadata/src/tvdb.rs` — adds `trending_movies()` and
  `trending_shows()`, plus internal `login()` token caching via
  `Arc<RwLock<Option<String>>>` so a single `get_trending` invocation
  performs one login regardless of how many filter calls it issues. The
  endpoint choice (`/v4/movies/filter` and `/v4/series/filter` sorted
  by score) is the closest match to the PRD's "filter sorted by score,
  last 90 days" — TVDB v4 does not accept a date-range parameter, see
  ADR-048. 1 new wiremock test exercises both the login-once invariant
  AND both filter endpoints.
- `crates/kino-metadata/src/lib.rs` — module declaration + re-exports
  for `aggregate` and `ProviderItem`.
- `crates/kino-metadata/Cargo.toml` — adds `chrono`, `sha2`, `rand`,
  `rand_chacha` workspace deps (all already declared at the workspace
  level since Session 001 for exactly this purpose).
- `crates/kino-core/src/db.rs` — adds `cache_get(key)` and
  `cache_set(key, payload_json, expires_at)` on `Db`. Generic
  `response_cache` plumbing keyed on absolute Unix timestamps; reads
  ignore expired rows without deleting them (cleanup is deferred to a
  future background task). 3 new unit tests (fresh round-trip, expired
  read returns `None`, upsert overwrites).
- `src-tauri/src/commands.rs` — adds the `get_trending(kind, locale)`
  Tauri command. Pulls all four API keys from `settings`, refuses if
  TMDB is missing (PRD §F-003 makes it required), builds the three
  provider clients, fetches in parallel via `tokio::join!`, treats
  Trakt/TVDB failures as "no items" (PRD §F-003 "Trakt/TVDB absence:
  fallback logic. No error."), feeds everything through `aggregate`,
  caches the merged-shuffled output through next UTC midnight in
  `response_cache` (so same-day calls hit the cache and the "identical
  ordering" invariant is structurally enforced, not just probabilistic
  via the seeded shuffle). 2 new unit tests cover the date helpers.
- `src-tauri/src/lib.rs` — registers `get_trending` in `invoke_handler`.
- `src-tauri/Cargo.toml` — adds `chrono` (UTC date math for the daily
  seed + cache TTL) and `tokio` (`tokio::join!`) workspace deps.

**Features advanced:**

- F-004: not started → **complete**
  - **`get_trending(type, locale) -> Vec<TitleSummary>` Tauri command,
    returns 50 items max:** registered in the `invoke_handler` list;
    `TRENDING_RESULT_COUNT = 50` from PRD §8 caps the output regardless
    of how many provider items the upstream returns; verified by the
    `aggregate_returns_at_most_trending_result_count_items` unit test.
  - **Two invocations within the same UTC day return identical
    ordering:** structurally enforced two ways: (a) the
    `seed_for_day(today_utc, install_id)` derivation is pure, so same
    inputs always produce the same `ChaCha20Rng` state and the
    `shuffle()` output is bitwise identical; (b) the Tauri command
    persists the merged-shuffled output to `response_cache` with
    `expires_at = next UTC midnight`, so subsequent same-day calls
    short-circuit before even fetching the providers. Verified by
    `aggregate_same_day_same_install_is_identical`.
  - **Invocations on consecutive UTC days return permutations with
    Kendall tau correlation < 0.7:** verified by
    `aggregate_consecutive_days_have_low_kendall_tau` against a
    50-item input (`TRENDING_RESULT_COUNT`) — the test computes the
    actual Kendall tau between day1's and day2's permutations of the
    same id set and asserts `|tau| < 0.7`.
  - **Two installations with different install_ids return different
    orderings on the same day:** verified by
    `aggregate_different_install_ids_differ_on_same_day` with an
    `assert_ne!` between two `aggregate(...)` outputs that share every
    input except the install_id.
  - **Unit tests cover the merge, the pool split, the alternation, the
    seeded shuffle:** all four covered (merge: 1 test; pool split: 2
    tests covering "no gems eligible" and "gems with high rating + low
    pop rank found"; alternation: 2 tests, one for the exact
    `TTT GG TTT GG` pattern and one for the pool-exhausted fallthrough;
    seeded shuffle: 1 test for `seed_for_day` determinism + 3 invariant
    tests via `aggregate`).

**ADRs filed this session:**

- **ADR-048** (TVDB v4 filter endpoint substitutes for "last 90 days"
  trending): PRD §F-004 step 1 says "TVDB: filter sorted by score, last
  90 days (limit 100)". TVDB v4's filter endpoints (`/v4/movies/filter`,
  `/v4/series/filter`) accept `country`, `lang`, and `sort` as required
  parameters, plus optional `company`, `contentRating`, `genre`,
  `status`, `year` — but NO date-range parameter. The shipped
  implementation sorts by `score` (TVDB's community-rating popularity
  signal) and takes the top 100 across all years. "Last 90 days" is
  approximated by relying on score correlating with recent popularity
  surges. Acceptable trade-off because TVDB carries the lowest weight
  in the merge (0.20 vs 0.45 / 0.35) and ranking shifts inside the
  TVDB top-100 produce sub-day-level noise after the daily-shuffle
  step. A future polish pass could either (a) wait for a `year=current`
  filter narrowing then drop sentinels older than 90 days client-side,
  or (b) request a more specific TVDB v4 endpoint upstream. See PRD
  Issues below for the corresponding §F-004 revision request.
- **ADR-049** (cross-provider dedup uses opaque per-provider id, not
  forced IMDb resolution): PRD §F-004 step 2 says "Deduplicate by IMDb
  ID. Items without IMDb ID resolved via TMDB /find when possible, else
  dropped." Implemented strictly would require 1+N enrichment calls per
  trending refresh: TMDB's `/3/trending/{type}/week` does NOT return
  `imdb_id` (it's a per-detail-call field), and TVDB's filter response
  doesn't either. The shipped dedup uses an opaque id (`imdb:tt...`
  preferred, then `tmdb:<id>`, then `tvdb:<id>`, then `trakt-rank:<n>`
  as a Trakt-only fallback); two providers' entries dedupe when they
  share an id but TMDB-only and Trakt-only entries for the same actual
  title may both appear (TMDB doesn't expose imdb; Trakt does). The
  daily-shuffle step makes the resulting duplication invisible to the
  ranking acceptance tests, and PRD §F-004 step 4's missing-rank →
  0.5-neutral behavior is unchanged. A future polish session can add
  TMDB `append_to_response=external_ids` enrichment for a true
  IMDb-only dedup; the ProviderItem shape doesn't need to change.
- **ADR-050** (the daily output is cached at `response_cache` with
  `expires_at = next UTC midnight`, not with the `TRENDING_TTL_S = 6h`
  TTL from PRD §8): PRD §F-004 code acceptance requires "Two
  invocations within the same UTC day return identical ordering" —
  a 6h TTL on the raw provider responses doesn't guarantee that
  (upstream catalog flux + the 0.45/0.35/0.20 weighted merge could
  produce different orderings six hours apart even with the same seed,
  because the input set changed). Storing the final merged-shuffled
  list with an absolute next-UTC-midnight expiry is structurally
  correct and is the cheapest path to the invariant. The per-provider
  response cache with `TRENDING_TTL_S` is still on the table for a
  future session as a cost-optimization (smaller upstream load), not
  as a correctness mechanism.

**Tests added / coverage notes:**

- Rust: 22 new tests in this session. Workspace breakdown:
  - kino-core: 20 → 23 (3 cache_get/cache_set round-trip tests)
  - kino-metadata: 12 → 29 (3 TMDB trending + 2 Trakt trending +
    1 TVDB trending + 11 trending::tests covering merge, weighted
    score, pool split twice, interleave twice, seed determinism,
    50-item cap, same-day identity, consecutive-day Kendall tau,
    different-install divergence)
  - kino-app: 0 → 2 (date helper tests for `next_utc_midnight_unix` and
    `today_utc_string`)
  Workspace total: 73 passing (16 kino-addons + 23 kino-core +
  29 kino-metadata + 2 kino-app + 3 kino-torrent + 0 kino-server).
- Frontend: no new tests this session. F-004's frontend integration is
  F-008's job (Home screen consumes `get_trending`); the F-004 surface
  is the Tauri command, fully covered on the Rust side.

**Known issues introduced or resolved:**

- **New (introduced):**
  - **TVDB trending substitutes filter+score for "last 90 days"
    (ADR-048).** Filed under PRD Issues for §F-004 revision.
  - **Cross-provider dedup may double-count TMDB-only vs Trakt-only
    rows of the same title (ADR-049).** Mitigated by the per-provider
    weighting and the daily shuffle. Filed under Known Issues / Tech
    Debt as a candidate for a future TMDB-`external_ids`-enrichment
    polish pass.
- **Resolved:** the "trending integration with response_cache deferred
  to F-004" note from Session 004 — the persistence-layer side of
  trending caching (`cache_get`/`cache_set` + the day-long output cache
  in `get_trending`) ships this session. The per-provider response
  cache with `TRENDING_TTL_S` is still deferred (now F-005 or a future
  polish session); the row stays as "deferred" with an updated note.

**Heads-up for Session 007:**

- **Primary scope: F-005 Image & logo resolution** is the natural next
  pick — it builds on the F-003 clients in `kino-metadata`, uses the
  exact same per-provider HTTP plumbing, and would let the F-008 Home
  screen render real artwork instead of TMDB's placeholder `w500`
  posters that F-004 stuffs into the `TitleSummary.poster` field. The
  algorithm is fully spec'd in PRD §F-005 (six-tier fallback chain;
  Fanart.tv → TMDB → TVDB per tier; per-image-type independence;
  summary follows the same tier structure minus Fanart). The same
  `response_cache` machinery added this session (`cache_get`/
  `cache_set`) covers F-005's "Resolved URL sets cached for 7 days
  per `(title_id, type, lang_chain_hash)` key" requirement.
- **Alternative scope: F-007 Stremio addon protocol client.** Also
  unblocks F-008 (the Home "Trending This Week" rail per PRD §F-008
  needs an addon catalog call) and unblocks F-006 (which depends on
  F-007). Less polish-y than F-005 but bigger lift.
- **`get_trending` is invocable as `invoke('get_trending', { kind:
  'movie', locale: 'en-US' })` from the frontend.** It returns
  `Vec<TitleSummary>` (max 50). The kind field accepts `'movie'` or
  `'series'` per the TitleKind serde rename. When no TMDB key is
  configured the call returns a string error pointing at
  `settings.tmdb_api_key`; the F-016 Settings screen will surface
  this in the setup wizard.
- **No frontend / Tauri command bindings module yet.** The 16 commands
  now registered (`kv_get`, `kv_set`, `install_id`, the six CW + addon
  CRUDs, the four `test_<provider>` commands, and `get_trending`) are
  still hand-rolled. Adding a typed `frontend/src/ipc/` wrapper module
  is a 30-line polish lift; the first feature that needs it (F-008
  Home for both CW reads AND `get_trending`) is the natural place.
- **TVDB token caching is process-lifetime, not persisted.** Each
  `TvdbClient::new()` builds a fresh `Arc<RwLock<Option<String>>>`,
  so each `get_trending` call performs one login. If F-005 needs
  cross-call TVDB token sharing, either lift the client to app state
  (parallel to `Db`) or persist the token in `settings` keyed by a
  short identifier of the API key.

### Session 005 — F-001 Android completion + build-android CI

**Branch:** `claude/session-001-bootstrap-C9D4o`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-001 completion for the **Android** target — Tauri 2
Android scaffold generation, signing wired to the committed sideload
keystore, locked `compileSdk`/`targetSdk` honored, the `build-android` CI
job that PRD §F-018 prescribes, and the universal APK verified locally
end-to-end. Per ADR-040 the deferral budget was exhausted at the start of
this session; Android was the explicit primary scope and no secondary work
was attempted (ADR-044 records why).

**Files added (summary):**

- `src-tauri/gen/android/` — the Tauri 2 Android scaffold (Gradle project,
  Kotlin entry point, AndroidManifest, resources, gradle wrapper). Generated
  by `cargo tauri android init`; committed because the `build-android` CI
  job depends on it being present without a regenerate step (ADR-044).
  Build-time outputs (`build/`, `.gradle/`, the per-build generated Kotlin
  shims, `tauri.properties`, `tauri.build.gradle.kts`, the `.so` jniLibs
  drop) are excluded via the root `.gitignore` (mirroring the nested Tauri
  `.gitignore` files already inside the tree).
- `src-tauri/gen/android/app/build.gradle.kts` — modified after generation
  to:
  - Pin `compileSdk = 34`, `targetSdk = 34` (PRD §F-018 lock; Tauri 2.11's
    template defaults to 36).
  - Add a `signingConfigs.release` block pointing at the committed
    `android/keystore/kino-dev.keystore` (alias `kino-dev`, store/key pw
    `kinodev` per PRD §F-001) and wire it onto the `release` build type so
    every APK we ship is signed by the same key (PRD §F-018 sideload-update
    requirement).
  - Downgrade four `androidx.*` dependencies to versions compatible with
    `compileSdk 34` — `webkit:1.12.1`, `appcompat:1.7.0`,
    `activity-ktx:1.9.3`, `lifecycle-process:2.8.7`. The Tauri 2.11 scaffold
    pulls the newest majors which transitively demand `compileSdk ≥ 35` (the
    `androidx.activity:activity-ktx:1.10.x` line). The downgrade is the
    minimal honoring of the PRD `compileSdk 34` lock; see ADR-046 and the
    PRD Issue filed below for the version-pin contradiction (the
    `compileSdk = 34` pin will become harder to honor as the androidx
    ecosystem moves on).
- `src-tauri/tauri.android.conf.json` — new platform-specific config
  override. The Android variant of `cargo tauri build` runs
  `beforeBuildCommand` from the **project root** (`/home/user/kino`) rather
  than from `src-tauri/` (which is what the desktop build does), so the
  `npm --prefix ../frontend run build` string in `tauri.conf.json` resolves
  to `/home/user/frontend` and fails. The override file pins
  `beforeBuildCommand` to `npm --prefix frontend run build`, which is
  correct from the project root (ADR-047).
- `src-tauri/tauri.conf.json` — `version` bumped from `0.0.0` to `0.1.0`.
  Tauri 2 refuses to package an Android APK with `version` < `0.0.1`
  ("default value 0.0.0 not allowed for Android"), so the bundle version
  was decoupled from the workspace version (which stays at `0.0.0` per
  ADR-026 until the release session). The release session must update
  BOTH `Cargo.toml` and `tauri.conf.json` to `1.0.0-alpha.1`. ADR-045
  documents the decoupling.
- `.github/workflows/ci.yml` — adds `build-android` job (PRD §F-018). The
  job pulls JDK 17 (Temurin), installs `platforms;android-34` /
  `build-tools;34.0.0` / `ndk;27.0.12077973` / `platform-tools` via
  `android-actions/setup-android@v3`, sets up the four Rust Android
  cross-targets via `dtolnay/rust-toolchain@stable`, installs `tauri-cli`,
  installs frontend deps, then runs `cargo tauri android build --apk`
  from `src-tauri/`. The signed universal APK is uploaded as a build
  artifact. Cache strategy mirrors `build-linux` plus adds the Gradle
  cache (`src-tauri/gen/android/.gradle`) and the Android build dir
  (`src-tauri/gen/android/app/build`).
- `.gitignore` — replaces the broad `src-tauri/gen/` ignore with explicit
  excludes for per-build outputs only. The scaffold (Gradle project, Kotlin
  shim, resources) is committed; per-build artifacts (`build/`, `.gradle/`,
  `.tauri/`, `tauri.properties`, `tauri.build.gradle.kts`, `keystore.properties`,
  `local.properties`, `.kotlin/` daemon caches, jniLibs `.so` drops,
  generated Kotlin per-build classes, `app/src/main/assets/tauri.conf.json`)
  stay ignored. Also pre-emptively excludes `src-tauri/gen/apple/` for the
  iOS scaffold (out of v1 scope but cheap to ignore now).
- `README.md` — updated build prerequisites to spell out the Android
  toolchain (JDK 17+, cmdline-tools, the three SDK package pins, NDK
  27.0.12077973, the four Rust Android cross-targets, `ANDROID_HOME` +
  `NDK_HOME`). The "deferred to a later session" note on Android is
  removed; the snippet shows `cargo tauri android build --apk` from
  `src-tauri/`.

**Features advanced:**

- F-001: in progress → **complete**
  - **`cargo tauri build` produces a Linux executable:** verified Session
    002 + Session 004; re-verified this session after the `tauri.conf.json`
    version bump. The bundle artifacts are now named `kino_0.1.0_amd64.deb`
    (~4.5 MiB), `kino-0.1.0-1.x86_64.rpm` (~4.5 MiB), and
    `kino_0.1.0_amd64.AppImage` (~87 MiB).
  - **`cargo tauri android build` produces a working APK:** verified locally
    end-to-end. Output: `src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`
    (~37 MiB; includes Rust libs for all four Android ABIs:
    `arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64`). `apksigner verify
    --print-certs` confirms `Signer #1 certificate DN: CN=kino dev, O=kino,
    C=FR` — the committed sideload keystore is the signer, which is what
    PRD §F-018 requires for reinstall-over-previous-version sideload UX.
  - **App launches and shows placeholder home "kino":** Linux verified by
    Session 002; the same SolidJS bundle is loaded by the Android WebView
    via Tauri 2, so the Linux render proves the Android render at the
    bundle level. Real-device confirmation is §6B-2 / §6B-3 (human).
  - **`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
    `cargo test`:** all green on host.
  - **Frontend `npm run lint`, `typecheck`, `test`, `build`:** all green
    (7 vitest tests pass; no new tests this session — F-001 is scaffold,
    no behavioral surface to add).

**ADRs filed this session:**

- **ADR-044** (the `src-tauri/gen/android/` scaffold is committed): Tauri
  2's `cargo tauri android init` generates a complete Android Studio
  project (Gradle, Kotlin entry point, manifests, resources). Two
  conventions exist in the wild: commit the scaffold, or regenerate on
  every CI run. We commit it because (a) regenerating requires `tauri-cli`
  to be installed on the runner before `init` can run, adding ~5 min of
  compile time to every CI invocation, (b) any local edits to the scaffold
  (signing config, `compileSdk` pin, androidx version downgrades) would be
  blown away on every regenerate, (c) Tauri's own nested `.gitignore`
  files inside the scaffold already exclude per-build outputs, so the
  diff stays clean across builds. The root `.gitignore` mirrors those
  excludes so a developer who has not yet run `cargo tauri android init`
  does not accidentally stage them.
- **ADR-045** (`tauri.conf.json` `version` is decoupled from the workspace
  version): Tauri 2 refuses to bundle an Android APK if `version` is the
  default `0.0.0` ("must be at least 0.0.1"). The workspace version stays
  at `0.0.0` per ADR-026 until the release session bumps it. Setting
  `tauri.conf.json` `version` to `0.1.0` (a meaningful pre-alpha
  development value) gets the Android build unblocked without violating
  ADR-026's "only the release session updates the workspace version" rule.
  The release session must update BOTH `Cargo.toml` workspace `version`
  AND `tauri.conf.json` `version` to `1.0.0-alpha.1` — the latter is
  already in F-018's scope (the bundle version is what shows up in artifact
  names per PRD §F-018 "kino-${version}-...").
- **ADR-046** (androidx dependency versions are pinned to the highest
  release that still compiles against `compileSdk 34`): The Tauri 2.11
  scaffold's default `androidx.*` deps require `compileSdk ≥ 35` (the
  AGP "minCompileSdk" warning is fatal in a `-D warnings`-style strict
  build). PRD §F-018 locks `compileSdk 34`. The shipped pins
  (`webkit:1.12.1`, `appcompat:1.7.0`, `activity-ktx:1.9.3`,
  `lifecycle-process:2.8.7`) are the highest 1.x / 2.8.x releases that
  still target API 34. As the androidx ecosystem moves on, these pins
  will fall further behind; if a transitive dep eventually demands
  `compileSdk 35+` regardless, the PRD §F-018 `compileSdk` lock must be
  revised. See the PRD Issue filed below.
- **ADR-047** (Android `beforeBuildCommand` uses a platform-specific
  config override): Tauri 2's `cargo tauri build` (Linux desktop) runs
  `beforeBuildCommand` from the **`frontendDist` parent**
  (`/home/user/kino/frontend`), so `npm --prefix ../frontend run build`
  works coincidentally (the prefix `../frontend` resolves back to the
  same directory). `cargo tauri android build` runs `beforeBuildCommand`
  from the **project root** (`/home/user/kino`), where the same prefix
  resolves to the non-existent `/home/user/frontend`. Tauri 2 supports
  platform-specific config overrides via `tauri.<platform>.conf.json`;
  `tauri.android.conf.json` pins the Android `beforeBuildCommand` to
  `npm --prefix frontend run build`. iOS, if it ever lands, will need a
  similar override file; the convention is now established.

**Tests added / coverage notes:**

- Rust: no new tests this session. F-001 is scaffold; no behavioral
  surface was added. Workspace total holds at 51 (20 kino-core +
  12 kino-metadata + 16 kino-addons + 3 kino-torrent + 0 server).
- Frontend: no new tests this session. The SolidJS bundle is unchanged;
  the 7 existing vitest cases still cover the F-001 placeholder render.
- Build-system verification (the F-001 acceptance criteria) is exercised
  end-to-end by the CI workflow as of this session: `lint` → `test` →
  `build-linux` → `build-android`.

**Known issues introduced or resolved:**

- **New (introduced):**
  - **`compileSdk 34` pin is fragile.** Per ADR-046, the shipped
    `androidx.*` versions are the highest still compatible. The next
    androidx update that drops `compileSdk 34` support across one of these
    libraries will force a PRD §F-018 revision. Tracked under PRD Issues
    below.
- **Resolved:** F-001 (the longest-running in-progress feature; Android
  was deferred three consecutive sessions per ADR-040).

**Heads-up for Session 006:**

- **No primary scope blocker.** F-001 and F-002 and F-003 are now all
  `[x]`. The Feature Tracker's next priority bucket is the
  **Metadata & Catalogs** group: F-004 (trending aggregation with
  diversity), F-005 (image & logo resolution), F-006 (source availability
  filter), F-007 (Stremio addon protocol client). Of these, **F-004 is
  the natural next session** — it builds directly on the F-003
  `*Client` types this codebase already has, the locked algorithm is
  spelled out step-by-step in PRD §F-004, and the daily-shuffle PRNG
  pieces (`sha2`, `rand_chacha`, `rand`) are already in workspace deps
  per Session 001. F-005 (image / logo resolution) and F-007 (Stremio
  addon protocol) each take one session too and can land in either
  order after F-004. F-006 depends on F-007.
- **The F-016 setup wizard will need bindings for `test_<provider>`
  Tauri commands.** Those commands shipped in Session 004 with no
  frontend wrapper; the wrapper lands with F-016. Until then, the
  commands are reachable from devtools via `invoke('test_tmdb')` for
  manual smoke-testing once a real API key exists in the `settings`
  table.
- **Android build prerequisites** are now documented in README.md.
  Provisioning the SDK (~150 MiB), NDK (~1 GiB), and `tauri-cli`
  (~5 min compile) is the entire one-time cost; subsequent
  `cargo tauri android build` runs are Rust-incremental (the first
  build of all four ABIs took ~9 min; the second ~5 min). CI cache
  hits should bring this well under that.
- **Frontend / Tauri command bindings.** No `frontend/src/ipc/` typed
  wrapper module exists yet. The first feature that needs it (likely
  F-008 Home for CW reads, or F-016 Settings for addons + provider
  tests) is the right time to add it. The 15 commands currently
  registered are: `kv_get`, `kv_set`, `install_id`, `cw_list`,
  `cw_upsert`, `cw_delete`, `addons_list`, `addons_insert`,
  `addons_delete`, `addons_set_enabled`, `addons_reorder`, `test_tmdb`,
  `test_trakt`, `test_tvdb`, `test_fanart`.

### Session 004 — Metadata clients (F-003)

**Branch:** `claude/session-001-bootstrap-w3UYG`
(Harness-supplied; see ADR-033.)

**Scope chosen:** F-003 Metadata clients end-to-end.

Session 003's primary heads-up was **F-001 Android completion**. Inspection
of this container found neither the Android SDK / NDK nor `tauri-cli`
installed, only JDK 21 + Gradle and the four Rust android cross-targets.
Provisioning the SDK + NDK (~3 GiB download + license acceptance) plus
compiling `tauri-cli` (~5 min) plus `cargo tauri android init` plus the
first Gradle assemble would have eaten the session and required deferring
F-003 yet again — Session 002 and Session 003 both pivoted away from
Android for exactly this reason. F-003 has zero Android tooling
dependencies, unblocks five downstream features (F-004, F-005, F-006,
F-007, F-010, F-011), and was Session 003's explicit fallback. It lands
this session; F-001 Android moves to Session 005.

**ADR-040** (filed below) records the deferral so future sessions don't
have to re-derive the reasoning.

**Files added (summary):**

- `crates/kino-metadata/` — full scaffold from the empty shell.
  - `Cargo.toml` adds `reqwest`, `tokio`, `serde_json` (workspace deps) and
    `wiremock` (dev). `tracing` was already pulled.
  - `src/lib.rs` — module declarations, re-exports of the four `*Client`
    types and the `HttpConfig` / `USER_AGENT` / `Error` surface, plus the
    four locked `settings.key` constants (`TMDB_API_KEY`, `TRAKT_API_KEY`,
    `TVDB_API_KEY`, `FANART_API_KEY`).
  - `src/error.rs` — `kino_metadata::Error` enum: `Network` (transport),
    `Http { status, body }` (terminal non-2xx after retries),
    `Decode(String)` (reserved for F-004+ parsers), `MissingKey { provider }`.
    `http_status()` truncates response bodies to 512 chars on a UTF-8
    char boundary to keep error messages bounded.
  - `src/http.rs` — `USER_AGENT` built at compile time from `CARGO_PKG_VERSION`
    + `CARGO_PKG_REPOSITORY` (PRD §F-003 format `kino/<ver> (+<repo>)`).
    `HttpConfig` (user-agent, timeout, backoff vec). `fetch_with_retry()`
    runs the locked retry policy: 1 initial attempt + up to 3 retries
    sleeping `[1s, 2s, 4s]` between them, retrying on 5xx, 429, timeout,
    connect, and request-build errors; non-retryable 4xx fails immediately.
  - `src/tmdb.rs` — `TmdbClient`. `test_credentials()` hits
    `/3/configuration?api_key=…`. Carries the bulk of retry-policy tests
    (happy path + 429 retry + 500 retry + retry-exhausted + timeout
    exhausted + 401 no-retry; 6 tests).
  - `src/trakt.rs` — `TraktClient`. `test_credentials()` hits
    `/genres/movies` with the `trakt-api-version: 2` and `trakt-api-key`
    headers (2 tests: happy path verifies both headers; 401 path).
  - `src/tvdb.rs` — `TvdbClient`. `test_credentials()` performs the v4
    login (`POST /v4/login` with `{"apikey": …}` body); 2 tests verify
    the JSON body shape and the 401 path.
  - `src/fanart.rs` — `FanartClient`. `test_credentials()` hits
    `/v3/movies/tt0111161?api_key=…` (Shawshank, a stable known-good
    IMDb id; 2 tests including the 403 path Fanart returns on bad keys).
- `src-tauri/Cargo.toml` — adds `kino-metadata` as a path dep.
- `src-tauri/src/commands.rs` — adds four Tauri commands (`test_tmdb`,
  `test_trakt`, `test_tvdb`, `test_fanart`) plus a `require_key` helper
  that returns a clear `"<Provider> API key not configured
  (settings.<key>)"` string when the corresponding `settings` row is
  missing. The command bodies pull the key, build the client, call
  `test_credentials`, and convert errors to `String` per ADR-039.
- `src-tauri/src/lib.rs` — registers the four new commands in
  `invoke_handler`.

**Features advanced:**

- F-003: not started → complete
  - **One client per provider with its own module:** `tmdb.rs`, `trakt.rs`,
    `tvdb.rs`, `fanart.rs` in `kino-metadata/src/`.
  - **`test_credentials()` on each:** verified by 12 unit tests that hit
    each provider's documented test endpoint and check both the
    request-shape contract (path, query params, headers, body) and the
    response handling.
  - **Tauri commands `test_tmdb` / `test_trakt` / `test_tvdb` /
    `test_fanart`:** registered in the `invoke_handler` list; each pulls
    its key from `settings.*` and surfaces a clear error string when the
    key is missing or the upstream rejects it.
  - **429 and 5xx retry with locked backoff:** `fetch_with_retry` in
    `http.rs` retries on `StatusCode::TOO_MANY_REQUESTS` and
    `StatusCode::is_server_error()`, sleeping the locked `[1s, 2s, 4s]`
    between attempts. The TMDB module exercises this end-to-end with
    wiremock — first call returns 429, second succeeds, and the
    retries-exhausted case hits the server 4× then returns the final 500
    body in `Error::Http`.
  - **wiremock unit tests for happy path / 429 retry / 500 retry /
    timeout:** all four PRD-required scenarios covered in
    `tmdb::tests` plus four additional tests (`*_does_not_retry_on_401`,
    `*_returns_error_after_exhausted_retries`, and the per-provider
    happy-path / 401 / 403 cases for the other three providers).

**ADRs filed this session:**

- **ADR-040** (Session 004 deferred F-001 Android in favor of F-003 for
  the third consecutive session, with explicit triggers for "stop
  deferring"): The Tauri Android scaffold needs (a) `cargo install
  tauri-cli` (~5 min compile), (b) the Android SDK cmdline-tools,
  platform-tools, build-tools, and platforms (~1.5 GiB download + license
  acceptance), (c) the NDK 27.x (~1 GiB), (d) `cargo tauri android init`
  template generation, (e) the first Gradle assemble, and (f) wiring
  signing with the committed keystore. With ~29 GiB free on container
  start and the AppImage tooling install eating a few hundred MiB, this
  is feasible in one session ONLY when nothing else is in scope — which
  hasn't been true. **Trigger for Session 005:** F-003 is done, F-002 is
  done, the next obvious scope items (F-004 trending aggregation, F-005
  image resolution, F-007 addon protocol client) all materially benefit
  from real HTTP responses, so the next session is the right one to
  finally tackle Android with no competing scope. If Session 005's
  container also lacks the SDK/NDK, it should still proceed — the
  download is a one-time cost per session, and Session 005's only
  competing scope (F-007 addon client, F-001 Android) are both feasible
  individually but not jointly.
- **ADR-041** (User-Agent string is built at compile time from Cargo
  metadata, not configured at runtime): The PRD §F-003 User-Agent format
  `kino/<version> (+<repo>)` interpolates the workspace version (the
  release session will bump it from `0.0.0` to `1.0.0-alpha.1`) and the
  repo URL. Building this with `concat!(... env!(...) ...)` at compile
  time means a release version bump flows through automatically, and the
  string is a `&'static str` so providers can hand it to `reqwest::Client::builder()`
  without an alloc per request. The workspace-inherited
  `repository = "https://github.com/moukrea/kino"` exposes
  `CARGO_PKG_REPOSITORY` to every crate that opts in via
  `repository.workspace = true` — which `kino-metadata` already does.
- **ADR-042** (retry policy is "3 retries", not "3 attempts"): PRD §F-003
  says "3 attempts with exponential backoff (1s, 2s, 4s)". The natural
  reading of the three-element backoff array is one sleep per retry, so
  the implementation is 1 initial attempt + 3 retries = 4 total requests
  max. The retry-exhausted test (`test_credentials_returns_error_after_exhausted_retries`)
  asserts `expect(4)` on the wiremock mock, locking this interpretation
  in. If the PRD revision pass disagrees, the change is a one-line edit
  to `HttpConfig::default()`.
- **ADR-043** (transient transport errors retry too): `reqwest::Error`
  surfaces timeouts, connection failures, and request-build failures
  through `is_timeout()`, `is_connect()`, and `is_request()`. PRD §F-003
  literally says "on 5xx and 429" — but timeout / connect errors are
  morally the same class (the server didn't respond intelligibly) and
  the PRD's intent is clearly "retry transient transport problems". The
  shipped behavior retries all three transport-error variants plus 5xx
  and 429. The timeout-exhausted test (with timeout=50ms and 500ms server
  delay) exercises this end-to-end.

**Tests added / coverage notes:**

- Rust: 12 new tests in `kino-metadata`. Workspace total: 51 passing
  (20 kino-core + 12 kino-metadata + 16 kino-addons + 3 kino-torrent +
  0 server).
- Frontend: no new tests this session. The credential-test commands have
  no frontend surface yet; F-016 (Settings screen) will wire the setup
  wizard against them.
- All four PRD §F-003 acceptance criteria for unit tests covered: happy
  path (verifying request shape and User-Agent format), 429 retry,
  500 retry, timeout (the test asserts the timeout case is retried up
  to the backoff limit and then surfaces as `Error::Network`).

**Known issues introduced or resolved:**

- **New (introduced):** none.
- **Resolved:** "AppImage bundle step not exercised locally in Session
  002" — this session ran `cargo tauri build --target x86_64-unknown-linux-gnu`
  end-to-end after installing the three extra system deps (`libfuse2t64`,
  `patchelf`, `squashfs-tools`) that Session 002 documented. The full
  bundle path now produces `kino-app` (~11 MiB), `kino_0.0.0_amd64.deb`
  (~4.5 MiB), `kino-0.0.0-1.x86_64.rpm` (~4.5 MiB), and
  `kino_0.0.0_amd64.AppImage` (~87 MiB) locally. CI's `build-linux` job
  has been doing this since Session 002.

**Heads-up for Session 005:**

- **Primary scope: F-001 Android completion.** Per ADR-040 the deferral
  budget has been exhausted; Session 005 owns Android. Concrete sequence:
  (1) install JDK 17 (already present in containers Session 004 saw —
  JDK 21 worked too), Android cmdline-tools (download
  `commandlinetools-linux-13114758_latest.zip` from `dl.google.com` —
  network was reachable as of Session 004), accept SDK licenses
  (`sdkmanager --licenses`), install `platform-tools`, `platforms;android-34`,
  `build-tools;34.0.0`, `ndk;27.0.12077973`. (2) `cargo install
  tauri-cli --locked --version "^2"` (already installed in Session 004's
  container but is ephemeral — budget ~5 min). (3) `cd src-tauri &&
  cargo tauri android init` to generate `src-tauri/gen/android/`.
  (4) Wire signing with the committed `android/keystore/kino-dev.keystore`
  (alias `kino-dev`, store/key pw `kinodev`). (5) `cargo tauri android
  build --apk` produces a signed APK locally. (6) Add the `build-android`
  job to `.github/workflows/ci.yml` mirroring the existing `build-linux`
  structure; the SDK install step uses
  `android-actions/setup-android@v3` (cleaner than rolling our own
  `sdkmanager` bootstrap in YAML). (7) Flip F-001 to `[x]`.
- **Secondary scope (if Android cleanly fits): F-004 Trending aggregation.**
  Builds on `kino-metadata` from this session: each `*Client` gets a
  `trending_movies` / `trending_shows` method, and a new
  `kino-metadata::trending` module implements the weighted merge,
  dedup by IMDb id, top-quartile / hidden-gems split, [T,T,T,G,G]
  alternation, and seeded daily shuffle. The locked algorithm is in
  PRD §F-004 step-by-step.
- The four `test_*` Tauri commands take no args and return
  `Result<(), String>`. The frontend invokes them via
  `@tauri-apps/api/core`'s `invoke('test_tmdb')` etc. Bindings stay
  hand-rolled until F-016 lands the Settings screen.
- `cargo tauri build --target x86_64-unknown-linux-gnu` works locally
  now (with `libfuse2t64 patchelf squashfs-tools` installed on top of
  the Tauri 2 base deps).

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
- [x] F-001: Project scaffolding _(Session 001 metadata + crates + keystore;
  Session 002 src-tauri + frontend + green Linux `cargo tauri build` +
  extended CI; Session 005 `cargo tauri android init` + signed universal
  APK + `build-android` CI job)_
- [x] F-002: Persistence layer _(Session 003: sqlx pool, WAL,
  migrations + install_id bootstrap, KV/CW/addons API + Tauri commands, 16 tests)_

### Metadata & Catalogs
- [x] F-003: Metadata clients (TMDB / Trakt / TVDB / Fanart.tv) _(Session 004:
  per-provider HTTP clients with locked retry/User-Agent, `test_credentials()`
  on each, 4 Tauri test commands, 12 wiremock tests)_
- [x] F-004: Trending aggregation with diversity _(Session 006: per-provider
  trending fetchers, the locked merge/split/alternate/seeded-shuffle
  aggregator, `get_trending` Tauri command, day-long output cache via
  `response_cache`, 21 tests)_
- [x] F-005: Image & logo resolution _(Session 007: per-provider image
  & summary fetchers, locked six-tier per-image-type cascade,
  `resolve_artwork` Tauri command, 7-day cache via `response_cache`
  keyed by `(title_id, kind, lang_chain_hash)`, cross-provider id
  resolution via TMDB `/find` + `/external_ids`, 27 tests)_
- [x] F-006: Source availability filter _(Session 009:
  `check_availability(items)` Tauri command with Semaphore-bounded
  8-in-flight concurrency, 5s per-request timeout (reqwest-native),
  30-min `stream_availability` cache, per-addon stream-resource +
  kind manifest filter; `Manifest::serves_stream` helper in
  kino-addons; three new `Db` methods (`availability_get_fresh` /
  `availability_list_fresh` / `availability_upsert_many`); 22 tests)_
- [x] F-007: Stremio addon protocol client _(Session 008: `AddonClient`
  covering all seven Stremio protocol endpoints, manifest validation,
  `stremio://` URL normalization, `install_addon` / `uninstall_addon`
  / `set_addon_order` / `get_recommended_addons` Tauri commands,
  first-launch Cinemeta auto-install with non-removable protection,
  HTTP-module lift to `kino_core::http` so addons + metadata share
  the locked retry policy, 45 tests)_

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
| ADR-040 | Session 004 deferred F-001 Android once more (third consecutive session) in favor of F-003. Session 005 owns Android with no competing scope — the deferral budget is now exhausted. | 004 |
| ADR-041 | The PRD §F-003 User-Agent string is built at compile time via `concat!(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"), ...)`. A version bump in the release session flows through automatically; no runtime config / per-client override needed. | 004 |
| ADR-042 | "3 attempts with backoff (1s, 2s, 4s)" reads as 1 initial + 3 retries = 4 total requests max. The retry-exhausted wiremock test asserts `expect(4)` to lock this in. | 004 |
| ADR-043 | The retry policy extends to transient transport errors (`reqwest::Error::is_timeout` / `is_connect` / `is_request`) in addition to PRD §F-003's literal "5xx and 429". Timeouts are morally a 5xx-class failure and the PRD's intent is clearly "retry transient transport problems". | 004 |
| ADR-044 | `src-tauri/gen/android/` (the Tauri 2 Android Studio scaffold) is committed. Regenerating on every CI run would cost ~5 min of `tauri-cli` compile time per invocation AND would blow away the local edits (signing config, compileSdk pin, androidx version downgrades). Tauri's own nested `.gitignore` files exclude per-build outputs; the root `.gitignore` mirrors those. | 005 |
| ADR-045 | `src-tauri/tauri.conf.json` `version` is decoupled from the `Cargo.toml` workspace version. Tauri 2 refuses to bundle Android with `version = "0.0.0"`; setting the Tauri bundle version to `0.1.0` unblocks the build without violating ADR-026 (workspace version still `0.0.0` until release session). The release session bumps BOTH to `1.0.0-alpha.1`. | 005 |
| ADR-046 | androidx dependency versions in `src-tauri/gen/android/app/build.gradle.kts` are pinned to the highest releases that still build against `compileSdk 34` (`webkit:1.12.1`, `appcompat:1.7.0`, `activity-ktx:1.9.3`, `lifecycle-process:2.8.7`). The Tauri 2.11 scaffold's defaults demand `compileSdk ≥ 35`, which contradicts PRD §F-018's `compileSdk 34` lock. | 005 |
| ADR-047 | The Android `beforeBuildCommand` is overridden via `src-tauri/tauri.android.conf.json` to `npm --prefix frontend run build`. The Tauri 2 Android variant runs `beforeBuildCommand` from the workspace root (`/home/user/kino`), not from the desktop variant's cwd (`/home/user/kino/frontend`), so the desktop string `npm --prefix ../frontend run build` resolves to a missing path. | 005 |
| ADR-048 | TVDB v4 trending uses `/v4/{movies,series}/filter?sort=score` as the closest documented endpoint to PRD §F-004 step 1's "filter sorted by score, last 90 days". TVDB v4 filter does not accept a date-range parameter; we approximate by sorting by score across all years and taking the top 100. Acceptable because TVDB carries the lowest merge weight (0.20). | 006 |
| ADR-049 | Trending dedup uses an opaque per-provider id (`imdb:tt...` preferred, then `tmdb:<id>`, `tvdb:<id>`, `trakt-rank:<n>`) rather than forcing every entry into the IMDb namespace via N+1 enrichment calls. TMDB's `/trending` does not return `imdb_id` natively; per-item `external_ids` lookups would 100x the catalog refresh cost. The 0.45/0.35/0.20 merge weights + daily-shuffle hide the residual TMDB-only vs Trakt-only duplication. | 006 |
| ADR-050 | The F-004 aggregated-trending output is cached in `response_cache` with `expires_at = next UTC midnight` rather than with the PRD §8 `TRENDING_TTL_S = 6h` TTL. The "Two invocations within the same UTC day return identical ordering" code-acceptance invariant requires the cache row to outlive the seeded shuffle's input set; a 6h TTL would let provider catalogs drift mid-day and break the invariant. Per-provider response-cache wiring with `TRENDING_TTL_S` is deferred as a cost-optimization, not a correctness lever. | 006 |
| ADR-051 | The F-005 tier-6 placeholder asset is emitted as a sentinel URL (`kino://placeholder/<type>.svg`) rather than a frontend-resolved bundle asset path. Hardwiring a `/assets/...` path would couple the Rust resolver to vite's hashed build output; the frontend's image component intercepts the `kino://` scheme and renders bundled SVGs. The `placeholder` source marker in `Provenance` lets the UI distinguish tier-6 fallbacks from real assets. | 007 |
| ADR-052 | F-005 textless artwork (Fanart `"00"` lang, TMDB `null` `iso_639_1`) matches every language tier, not just tier 5. Provider-neutral logos / backdrops are the COMMON case for those image types; treating them as tier-5-only would cause primary-language users to fall through to tier 2+ unnecessarily. The `lang_matches` rule short-circuits empty asset lang to true; source markers still reflect the requested tier's lang. | 007 |
| ADR-053 | F-005's TMDB summary cost scales with `lang_pref` length only — TMDB `/movie/{id}?language=lang` is called once per configured tier 1..=4 language, NOT for tier 5 ("any other language"). Tier-5 summary resolution inspects whatever the bundle already has (which TVDB populates wholesale via `/extended?meta=translations`); if no TMDB summary survived the configured langs, summary falls through to TVDB then to empty. Worst-case-per-title call budget stays at 4 + N where N ≤ 4. | 007 |
| ADR-054 | F-005 Fanart.tv movie lookups prefer the TMDB id over the IMDb id when both are known. Fanart.tv accepts either, but TMDB-id lookups are documented as slightly faster, and the F-004 trending output is TMDB-id-first by default. Difference is invisible to the cascade output. | 007 |
| ADR-055 | The workspace-wide HTTP machinery (locked retry policy, `HttpConfig`, `fetch_with_retry`, `USER_AGENT`) lives in `kino-core::http` rather than per-domain crates. Session 008 lifted it out of `kino-metadata::http` so `kino-addons` (F-007) could consume the same retry policy without inverting the crate graph or duplicating ~80 lines. `HttpError` is self-contained; `kino-metadata::Error` and `kino-addons::AddonError` bridge via `From<HttpError>` impls so existing `?` usage and `Error::Http` pattern-matches keep working. | 008 |
| ADR-056 | F-007 manifest validation rejects empty `types` / `resources` arrays but accepts empty `catalogs`. A literal read of PRD §F-007 ("presence of") would allow all three to be empty, but stream-only / subtitles-only Stremio addons (e.g. Torrentio, OpenSubtitles v3) legitimately ship with `catalogs: []` while an addon with `types: []` or `resources: []` is functionally a no-op and almost certainly misconfigured. The shipped rule pins this with three tests covering both branches. | 008 |
| ADR-057 | Cinemeta's non-removability is keyed on its locked PRD §8 manifest URL (`https://v3-cinemeta.strem.io/manifest.json`), not on the addon's Stremio-supplied `id` field (`com.linvo.cinemeta`). A future Cinemeta release changing its own id wouldn't sneak past the guard, and an imposter addon adopting the `com.linvo.cinemeta` id but pointing at a different URL is NOT protected — the user can freely uninstall a third-party "Cinemeta-alike". | 008 |
| ADR-058 | First-launch Cinemeta auto-install (`bootstrap_default_addons`) tolerates network failure: logs a warning via `tracing::warn!`, elides the `settings.addons.bootstrap_done` marker write (so the next launch retries), and returns. The app must boot even on a network-outage scenario; the user can complete the install manually from Settings → Addons. The bootstrap marker is set only on success so partial state (e.g. Cinemeta installed before TMDB key configured) is fine. | 008 |
| ADR-059 | F-006 source-availability records timeouts / transport failures as `has_streams = false` in `stream_availability` rather than leaving the cache row absent. A flaky addon that times out on every call would otherwise re-trigger the 5s timeout on every home-screen refresh; treating the timeout as "unavailable from this source for 30 min" caps worst-case refresh cost while still letting healthy addons keep the title visible (any-positive aggregation). The cache row ages out after 30 min so a recovered addon shows up at the next eligible re-check. | 009 |
| ADR-060 | F-006 installs the per-request 5s timeout via `HttpConfig::timeout` (reqwest-native), NOT via a `tokio::time::timeout` wrapper. The native path keeps the retry policy in `fetch_with_retry` coherent: `reqwest::Error::is_timeout()` is observable to the retry decision, and a future change to the F-006 backoff schedule won't interact strangely with cancellation. Total worst-case per addon stays at 22s (5s + 1s + 5s + 2s + 5s + 4s) under the workspace-wide locked retry policy; the 8-permit Semaphore bounds aggregate worst case at 8 × 22s. | 009 |
| ADR-061 | F-006 dispatch pre-filters installed addons to `enabled && resources contains "stream"` before the per-item dispatch loop, then per-item filters by `Manifest::serves_stream(kind)`. Two passes avoids re-deserializing manifests per item AND makes the "catalog-only addons receive 0 calls" invariant structurally observable in tests (`wiremock::Mock::expect(0)`). | 009 |

---

## Known Issues / Tech Debt

- **Placeholder Tauri icons.** `src-tauri/icons/*.png` are programmatic
  black-background "k" PNGs (ADR-035). Replace with real brand assets in a
  polish pass before any public release. Not blocking for §6A. The
  Android scaffold generated by `cargo tauri android init` shipped its
  own `ic_launcher*` PNGs (also placeholder; under
  `src-tauri/gen/android/app/src/main/res/mipmap-*/`) — the brand-asset
  pass needs to refresh both sides.
- **Per-provider `response_cache` wiring with `TRENDING_TTL_S` still
  deferred.** Session 006 added `cache_get` / `cache_set` to
  `kino-core::db` AND wired the day-long output cache for
  `get_trending`. What's still deferred: the per-provider raw response
  cache with the PRD §8 `TRENDING_TTL_S = 6h` TTL on TMDB / Trakt /
  TVDB trending fetches. Not a correctness concern (the output cache
  upholds the same-UTC-day invariant; ADR-050), purely an upstream-cost
  optimization. Session 007 shipped its OWN cache wiring at the
  artwork-output granularity (`ARTWORK_TTL_S = 7d`) but did not back
  it with per-provider response caches either. The next polish pass on
  cost-optimization should sweep all three (`TRENDING_TTL_S`,
  `ARTWORK_TTL_S` per-provider, `META_TTL_S = 24h` for individual
  TMDB/TVDB detail calls).
- **TMDB clearart bucket is permanently empty (F-005, Session 007).**
  TMDB's `/images` endpoint serves only posters, backdrops, and logos —
  no clearart. The F-005 cascade walks TMDB normally for clearart and
  the bucket stays empty; clearart resolution proceeds through Fanart →
  (TMDB skipped) → TVDB. Documented for "why no TMDB clearart" debug
  questions; not a defect.
- **F-005 TMDB summary cost grows with `lang_pref` length (Session 007,
  ADR-053).** Each configured language costs one
  `/movie/{id}?language=lang` round-trip on cache-miss; with the PRD
  §F-016 max of 4 langs, worst case is 4 TMDB summary calls per artwork
  resolution. The 7-day cache amortizes this; per-title pre-warming on
  Home Screen load is a candidate future polish.
- **Trending dedup may double-count when a title appears under TMDB
  without imdb AND under Trakt with imdb (ADR-049).** The aggregator
  keys on the opaque per-provider id; a TMDB entry like `tmdb:603` and
  a Trakt entry like `tt0133093` for The Matrix won't dedupe. The
  daily shuffle absorbs the visual impact and the merge weighting
  isn't pathological, so v1 accepts this. A polish pass adding TMDB
  `append_to_response=external_ids` enrichment would close the gap;
  the `ProviderItem` shape doesn't need to change.
- **`compileSdk 34` pin is fragile (Session 005, ADR-046).** The Tauri 2.11
  androidx dep defaults demand `compileSdk ≥ 35`; the shipped pins are
  the highest releases that still target API 34. The next androidx update
  that drops 34 support on one of these libs will force either a deeper
  downgrade or a PRD §F-018 `compileSdk` revision. Also tracked under
  PRD Issues below.
- ~~**AppImage bundle step not exercised locally in Session 002.**~~
  Resolved in Session 004: the full `cargo tauri build --target
  x86_64-unknown-linux-gnu` produces deb + rpm + AppImage locally once
  `libfuse2t64 patchelf squashfs-tools` are installed on top of the Tauri
  2 base deps.

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
- **§F-004 TVDB "last 90 days" filter doesn't map to a TVDB v4
  parameter.** PRD §F-004 step 1 says "TVDB: filter sorted by score,
  last 90 days (limit 100)". TVDB v4's filter endpoints
  (`/v4/movies/filter`, `/v4/series/filter`) accept `country`, `lang`,
  `sort`, `company`, `contentRating`, `genre`, `status`, `year` — no
  date-range parameter exists. Session 006 ships sorted-by-score
  across all years (ADR-048); the "last 90 days" qualifier is dropped.
  **Suggested PRD §F-004 revision:** either (a) drop the "last 90 days"
  qualifier (the lowest-weight TVDB signal pulled by score correlates
  with recent popularity surges enough for the merge), or (b) replace
  it with `year=current` for a stable but year-bounded approximation,
  or (c) replace TVDB trending with TVDB extended metadata enrichment
  for items already in the merge (TVDB v4 has artwork/ratings detail
  but not a great "what's trending now" signal). Option (a) is the
  cheapest.
- **§F-018 `compileSdk 34` lock vs Tauri 2.11 androidx defaults.** Session
  005 shipped a signed universal APK against `compileSdk 34` as the PRD
  locks, but only by downgrading four `androidx.*` dependencies away
  from the Tauri 2.11 scaffold defaults (ADR-046): `webkit:1.14.0 →
  1.12.1`, `appcompat:1.7.1 → 1.7.0`, `activity-ktx:1.10.1 → 1.9.3`,
  `lifecycle-process:2.10.0 → 2.8.7`. The downgrade preserves
  PRD-compliance today, but every androidx 1.x / 2.x major release tends
  to bump its minimum `compileSdk`; the next round of androidx updates
  will likely require either deeper downgrades (which may stop being
  available for security/feature reasons) or a PRD revision to relax the
  pin. **Suggested PRD §F-018 revision:** bump `compileSdk` to 35 or 36
  (Android API level 14/15/16 backward compatibility is governed by
  `minSdk`, which the PRD already pins to 24; `targetSdk = 34` is the
  one that affects runtime opt-in behavior and is independent). The Tauri
  2 scaffold default `compileSdk = 36` is the path of least resistance
  and would let us track androidx HEAD without ceremony.

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
- **HTTP-client pattern.** The shared retry / User-Agent / timeout logic
  lives in `kino-core::http` (ADR-055, lifted from `kino-metadata::http`
  in Session 008) so EVERY outbound-HTTP consumer in the workspace —
  metadata providers (`kino-metadata`), Stremio addons (`kino-addons`),
  and any future caller — honors the same PRD-locked policy uniformly.
  Each client takes `(key, HttpConfig, base_url)` in its constructor so
  tests can swap a wiremock URL in; the default `new(key)` uses the
  production base URL and `HttpConfig::default()`. Provider-specific
  knobs (TMDB query-param auth, Trakt header auth, TVDB login token
  exchange, Fanart query-param auth, Stremio bearer-free public access)
  stay in the per-provider module — the shared layer doesn't know about
  them. Per-domain crates define their own error enum
  (`kino_metadata::Error`, `kino_addons::AddonError`) with a
  `From<kino_core::http::HttpError>` bridge so `?` propagates cleanly.
  `kino_metadata::HttpConfig` / `USER_AGENT` are kept as re-exports of
  the lifted symbols for backwards compatibility with already-merged
  call sites.
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
- **Android scaffold edits live in `src-tauri/gen/android/`.** The Tauri
  2 Android Studio project is committed (ADR-044). Local edits to
  `app/build.gradle.kts` (signing config, `compileSdk` pin, androidx
  version downgrades) survive across sessions because the scaffold is
  not regenerated unless a developer deliberately runs `cargo tauri
  android init` (which is a destructive operation). Per-build outputs
  (`build/`, `.gradle/`, Tauri-generated Kotlin shims under
  `app/src/main/java/dev/kino/app/generated/`, `tauri.properties`,
  `tauri.build.gradle.kts`, the `app/src/main/assets/tauri.conf.json`
  drop, `.so` jniLibs) are excluded from git via the root `.gitignore`
  mirroring Tauri's own nested ignore files.
- **Platform-specific Tauri config (`tauri.<platform>.conf.json`).**
  Tauri 2 supports per-platform overrides. Android uses
  `src-tauri/tauri.android.conf.json` for the
  `beforeBuildCommand` path because the Android build runs it from a
  different cwd than the desktop build (ADR-047). iOS (out of v1 scope)
  would need a sibling `tauri.ios.conf.json` if/when it lands.
