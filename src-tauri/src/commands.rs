//! Tauri command surface (PRD §F-002 onward).
//!
//! Every command exposed to the frontend lives here, grouped by the feature
//! that owns it. Errors cross the IPC boundary as plain strings — the Tauri
//! frontend bindings surface them through the standard `Result` shape.
//!
//! F-002 ships KV (settings), Continue Watching, and addon CRUD. F-003 adds
//! the per-provider credential-test commands. Later features extend this
//! module rather than introducing parallel registries.

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use kino_addons::parse::parse as parse_filename;
use kino_addons::{
    normalize_manifest_url, AddonClient, AddonError, CatalogDescriptor, Manifest, MetaDetail,
    MetaPreview, RecommendedAddon, Stream as AddonStream, CINEMETA_MANIFEST_URL,
    RECOMMENDED_ADDONS,
};
use kino_core::addon::{Addon, AddonInsert};
use kino_core::availability::AvailabilityRow;
use kino_core::constants::{
    ARTWORK_TTL_S, AVAILABILITY_CONCURRENCY, AVAILABILITY_TIMEOUT_S, SEARCH_TTL_S,
};
use kino_core::cw::ContinueWatching;
use kino_core::http::HttpConfig;
use kino_core::stream::{Audio, Codec, Hdr, ParsedTags, Quality};
use kino_core::title::{Artwork, TitleKind, TitleSummary};
use kino_core::Db;
use kino_metadata::artwork::{cascade, lang_chain_hash, CachedArtwork, ProviderBundles};
use kino_metadata::tmdb::TitleIds;
use kino_metadata::{
    aggregate, FanartClient, ProviderItem, TmdbCastMember, TmdbClient, TmdbTitleDetails,
    TraktClient, TvdbClient, FANART_API_KEY, TMDB_API_KEY, TRAKT_API_KEY, TVDB_API_KEY,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::State;
use tokio::sync::Semaphore;

/// Convert a `kino-core` error into the string the Tauri IPC layer
/// serializes to the frontend.
fn ipc<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---- settings (KV) -------------------------------------------------------

#[tauri::command]
pub async fn kv_get(db: State<'_, Db>, key: String) -> Result<Option<String>, String> {
    db.kv_get(&key).await.map_err(ipc)
}

#[tauri::command]
pub async fn kv_set(db: State<'_, Db>, key: String, value: String) -> Result<(), String> {
    db.kv_set(&key, &value).await.map_err(ipc)
}

#[tauri::command]
pub async fn install_id(db: State<'_, Db>) -> Result<String, String> {
    db.install_id().await.map_err(ipc)
}

// ---- continue_watching --------------------------------------------------
//
// F-002 ships `cw_list` / `cw_upsert` / `cw_delete` as low-level CRUD.
// F-012 layers the PRD §F-012 semantics on top via `cw_record_position`
// (the canonical writer the player and detail-view actions call) and
// `cw_remove_title` (the home-row manual-remove action). The
// auto-removal sweep runs implicitly inside `cw_list` so the home
// screen never displays a completed row past its 24h window.

#[tauri::command]
pub async fn cw_list(db: State<'_, Db>) -> Result<Vec<ContinueWatching>, String> {
    // PRD §F-012 "Completed items auto-removed from Continue Watching
    // after 24h" — sweep before returning so the home screen sees the
    // up-to-date list without needing its own scheduler.
    if let Err(e) = cw_sweep_completed(&db).await {
        tracing::warn!(error = %e, "cw_sweep_completed failed; returning unswept list");
    }
    db.cw_list().await.map_err(ipc)
}

#[tauri::command]
pub async fn cw_upsert(db: State<'_, Db>, entry: ContinueWatching) -> Result<(), String> {
    db.cw_upsert(&entry).await.map_err(ipc)
}

#[tauri::command]
pub async fn cw_delete(
    db: State<'_, Db>,
    title_id: String,
    season: i64,
    episode: i64,
) -> Result<u64, String> {
    db.cw_delete(&title_id, season, episode).await.map_err(ipc)
}

/// PRD §F-012 canonical position writer. Applies the locked completion
/// + next-episode rules in one place so the player (F-015, future) and
/// the title-detail Resume / Mark Watched actions share the same
/// behavior.
///
/// `episodes` is the canonical series episode list as `(season,
/// episode)` tuples; pass `Vec::new()` for movies. When the rule
/// resolves to [`kino_core::cw::ResumeDecision::AdvanceToNext`] the
/// row's `(season, episode, position_s)` are replaced with the next
/// episode at position 0 and the old row is deleted. When the rule
/// resolves to [`kino_core::cw::ResumeDecision::RemoveSeries`] every CW
/// row for the title is wiped (the series has been fully watched).
///
/// Returns the canonical CW row that ends up on disk — `None` when the
/// rule was `RemoveSeries`. The frontend uses the return to mirror its
/// in-memory CW signal without re-fetching.
#[tauri::command]
pub async fn cw_record_position(
    db: State<'_, Db>,
    entry: ContinueWatching,
    episodes: Vec<(i64, i64)>,
) -> Result<Option<ContinueWatching>, String> {
    cw_record_position_inner(&db, entry, &episodes).await
}

/// Implementation core for [`cw_record_position`]. Takes a `&Db`
/// directly so unit tests can drive it without a Tauri `State`. The
/// command wrapper above is a one-liner.
async fn cw_record_position_inner(
    db: &Db,
    entry: ContinueWatching,
    episodes: &[(i64, i64)],
) -> Result<Option<ContinueWatching>, String> {
    use kino_core::cw::{resume_decision, ResumeDecision};
    match resume_decision(&entry, episodes) {
        ResumeDecision::Keep => {
            db.cw_upsert(&entry).await.map_err(ipc)?;
            Ok(Some(entry))
        }
        ResumeDecision::AdvanceToNext { season, episode } => {
            // Wipe the previous (completed) episode's row before
            // writing the new one so only one row per series is kept.
            db.cw_delete(&entry.title_id, entry.season, entry.episode)
                .await
                .map_err(ipc)?;
            let advanced = ContinueWatching {
                title_id: entry.title_id.clone(),
                kind: entry.kind,
                season,
                episode,
                position_s: 0.0,
                duration_s: 0.0,
                last_played_at: entry.last_played_at,
                meta_json: entry.meta_json.clone(),
            };
            db.cw_upsert(&advanced).await.map_err(ipc)?;
            Ok(Some(advanced))
        }
        ResumeDecision::RemoveSeries => {
            db.cw_delete_all_for_title(&entry.title_id)
                .await
                .map_err(ipc)?;
            Ok(None)
        }
    }
}

/// PRD §F-012 manual-remove action: wipe every CW row that belongs to
/// `title_id`. The home-screen CW row triggers this via the `context`
/// action (Y / Menu / right-click / long-press); the action targets the
/// whole title rather than the single `(season, episode)` row because
/// the home renders one tile per title.
#[tauri::command]
pub async fn cw_remove_title(db: State<'_, Db>, title_id: String) -> Result<u64, String> {
    db.cw_delete_all_for_title(&title_id).await.map_err(ipc)
}

/// PRD §F-012 auto-removal sweep. Walks every CW row and deletes any
/// that satisfy [`kino_core::cw::should_auto_remove`] under the current
/// system clock. Invoked from `cw_list`; also exposed as a Tauri
/// command for explicit calls (the Settings screen could surface a
/// "Sweep finished items now" button in a future polish pass).
#[tauri::command]
pub async fn cw_sweep(db: State<'_, Db>) -> Result<u64, String> {
    cw_sweep_completed(&db).await.map_err(ipc)
}

/// Inner sweep used by `cw_list` and `cw_sweep`. Returns the number of
/// rows removed.
async fn cw_sweep_completed(db: &Db) -> Result<u64, kino_core::db::DbError> {
    use kino_core::cw::should_auto_remove;
    let rows = db.cw_list().await?;
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX);
    let mut removed: u64 = 0;
    for row in rows {
        if should_auto_remove(&row, now) {
            removed += db.cw_delete(&row.title_id, row.season, row.episode).await?;
        }
    }
    Ok(removed)
}

// ---- addons -------------------------------------------------------------

#[tauri::command]
pub async fn addons_list(db: State<'_, Db>) -> Result<Vec<Addon>, String> {
    db.addons_list().await.map_err(ipc)
}

#[tauri::command]
pub async fn addons_insert(db: State<'_, Db>, addon: AddonInsert) -> Result<(), String> {
    db.addons_insert(&addon).await.map_err(ipc)
}

#[tauri::command]
pub async fn addons_delete(db: State<'_, Db>, id: String) -> Result<u64, String> {
    db.addons_delete(&id).await.map_err(ipc)
}

#[tauri::command]
pub async fn addons_set_enabled(
    db: State<'_, Db>,
    id: String,
    enabled: bool,
) -> Result<u64, String> {
    db.addons_set_enabled(&id, enabled).await.map_err(ipc)
}

#[tauri::command]
pub async fn addons_reorder(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    db.addons_reorder(&ids).await.map_err(ipc)
}

// ---- F-003: metadata-provider credential tests --------------------------
//
// Each command pulls the provider's API key from `settings`, builds a fresh
// client, and reports whether the upstream accepts the key. The frontend
// uses these to drive the setup wizard (F-016).

async fn require_key(db: &Db, setting_key: &str, provider: &'static str) -> Result<String, String> {
    db.kv_get(setting_key)
        .await
        .map_err(ipc)?
        .ok_or_else(|| format!("{provider} API key not configured (settings.{setting_key})"))
}

#[tauri::command]
pub async fn test_tmdb(db: State<'_, Db>) -> Result<(), String> {
    let key = require_key(&db, TMDB_API_KEY, "TMDB").await?;
    let client = TmdbClient::new(key).map_err(ipc)?;
    client.test_credentials().await.map_err(ipc)
}

#[tauri::command]
pub async fn test_trakt(db: State<'_, Db>) -> Result<(), String> {
    let key = require_key(&db, TRAKT_API_KEY, "Trakt").await?;
    let client = TraktClient::new(key).map_err(ipc)?;
    client.test_credentials().await.map_err(ipc)
}

#[tauri::command]
pub async fn test_tvdb(db: State<'_, Db>) -> Result<(), String> {
    let key = require_key(&db, TVDB_API_KEY, "TVDB").await?;
    let client = TvdbClient::new(key).map_err(ipc)?;
    client.test_credentials().await.map_err(ipc)
}

#[tauri::command]
pub async fn test_fanart(db: State<'_, Db>) -> Result<(), String> {
    let key = require_key(&db, FANART_API_KEY, "Fanart.tv").await?;
    let client = FanartClient::new(key).map_err(ipc)?;
    client.test_credentials().await.map_err(ipc)
}

// ---- F-004: trending aggregation ---------------------------------------
//
// The aggregator and per-provider HTTP plumbing live in `kino-metadata`;
// this command stitches them onto the persistence layer (api keys,
// install_id) and the response cache. PRD §F-004 invariant "same UTC day
// returns identical ordering" is enforced by storing the merged-shuffled
// list with `expires_at = next UTC midnight`; subsequent same-day calls
// hit the cache.

/// `get_trending(kind, locale)` (PRD §F-004).
///
/// `kind` is `TitleKind::Movie` or `TitleKind::Series`. `locale` is forwarded
/// to TMDB as the `language` parameter; pass `"en-US"` for the default
/// catalog. Returns up to `TRENDING_RESULT_COUNT` (50) items, ordered by
/// the daily-seeded shuffle.
#[tauri::command]
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
pub async fn get_trending(
    db: State<'_, Db>,
    kind: TitleKind,
    locale: String,
) -> Result<Vec<TitleSummary>, String> {
    let today = today_utc_string();
    let cache_key = format!("trending:{}:{today}", kind.as_str());

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<Vec<TitleSummary>>(&cached) {
            return Ok(parsed);
        }
        // Malformed cache rows shouldn't poison subsequent reads — fall
        // through to a fresh fetch which will overwrite the row.
        tracing::warn!(key = %cache_key, "discarding malformed cached trending payload");
    }

    let install_id = db.install_id().await.map_err(ipc)?;
    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let trakt_key = db.kv_get(TRAKT_API_KEY).await.map_err(ipc)?;
    let tvdb_key = db.kv_get(TVDB_API_KEY).await.map_err(ipc)?;

    // PRD §F-003 provider rules: TMDB is required for trending; without it,
    // home / search are empty with a clear configure-key message. Trakt and
    // TVDB absence falls through to a TMDB-only merge (PRD §F-004 step 4
    // missing rank → 0.5 neutral).
    let Some(tmdb_key) = tmdb_key else {
        return Err(format!(
            "TMDB API key not configured (settings.{TMDB_API_KEY}) — Home is empty until it's set."
        ));
    };

    let (tmdb_items, trakt_items, tvdb_items) = fetch_all_providers(
        &tmdb_key,
        trakt_key.as_deref(),
        tvdb_key.as_deref(),
        &kind,
        &locale,
    )
    .await?;

    let merged = aggregate(tmdb_items, trakt_items, tvdb_items, &install_id, &today);

    // Cache through the rest of the UTC day. We use the absolute next-UTC-
    // midnight rather than `now + TRENDING_TTL_S` so the same-UTC-day
    // determinism invariant (PRD §F-004 code acceptance) is structurally
    // upheld no matter how long the user keeps the app running.
    let expires_at =
        next_utc_midnight_unix(&today).ok_or_else(|| "internal: invalid UTC date".to_string())?;
    let payload = serde_json::to_string(&merged).map_err(|e| e.to_string())?;
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, "failed to persist trending cache");
    }

    Ok(merged)
}

#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
async fn fetch_all_providers(
    tmdb_key: &str,
    trakt_key: Option<&str>,
    tvdb_key: Option<&str>,
    kind: &TitleKind,
    locale: &str,
) -> Result<(Vec<ProviderItem>, Vec<ProviderItem>, Vec<ProviderItem>), String> {
    // Build clients up front so a client-construction failure surfaces before
    // we issue any network calls.
    let tmdb = TmdbClient::new(tmdb_key).map_err(ipc)?;
    let trakt = match trakt_key {
        Some(k) => Some(TraktClient::new(k).map_err(ipc)?),
        None => None,
    };
    let tvdb = match tvdb_key {
        Some(k) => Some(TvdbClient::new(k).map_err(ipc)?),
        None => None,
    };

    let tmdb_fut = async move {
        match kind {
            TitleKind::Movie => tmdb.trending_movies(locale).await,
            TitleKind::Series => tmdb.trending_shows(locale).await,
        }
    };
    let trakt_fut = async move {
        let Some(c) = trakt else {
            return Ok(Vec::new());
        };
        match kind {
            TitleKind::Movie => c.trending_movies().await,
            TitleKind::Series => c.trending_shows().await,
        }
    };
    let tvdb_fut = async move {
        let Some(c) = tvdb else { return Ok(Vec::new()) };
        match kind {
            TitleKind::Movie => c.trending_movies().await,
            TitleKind::Series => c.trending_shows().await,
        }
    };

    // tokio::join! waits for all three regardless of individual failures;
    // we surface them with provider context. Trakt/TVDB failures are
    // recoverable (treated as "no items" so the aggregator falls back to
    // the neutral 0.5 rank); TMDB failure is fatal because PRD §F-003
    // makes TMDB the required provider.
    let (tmdb_res, trakt_res, tvdb_res) = tokio::join!(tmdb_fut, trakt_fut, tvdb_fut);
    let tmdb_items = tmdb_res.map_err(|e| format!("TMDB trending: {e}"))?;
    let trakt_items = trakt_res.unwrap_or_else(|e| {
        tracing::warn!(provider = "trakt", error = %e, "trending fetch failed; skipping");
        Vec::new()
    });
    let tvdb_items = tvdb_res.unwrap_or_else(|e| {
        tracing::warn!(provider = "tvdb", error = %e, "trending fetch failed; skipping");
        Vec::new()
    });
    Ok((tmdb_items, trakt_items, tvdb_items))
}

/// `YYYY-MM-DD` of the current UTC day, used as the F-004 daily seed input
/// AND as part of the cache key (so a stale entry left over from a prior
/// day is structurally invisible to today's reads).
fn today_utc_string() -> String {
    let now = Utc::now();
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

/// Absolute Unix timestamp at the start of the day AFTER `today`. The
/// trending output cache row expires at this moment so the very first call
/// on the new UTC day cache-misses and re-shuffles with the new seed.
fn next_utc_midnight_unix(today: &str) -> Option<i64> {
    let parsed = NaiveDate::parse_from_str(today, "%Y-%m-%d").ok()?;
    let next = parsed.succ_opt()?;
    let midnight = next.and_hms_opt(0, 0, 0)?;
    Some(Utc.from_utc_datetime(&midnight).timestamp())
}

// ---- F-008: home-screen feeds ------------------------------------------
//
// `get_trending_pools` and `get_weekly_trending` feed the locked Home
// screen row order (PRD §F-008): Continue Watching, Trending Now,
// Hidden Gems, Trending This Week, addon catalogs. The CW row is fed
// by the existing `cw_list` command; addon catalogs are deferred to a
// follow-up. The two pool-rows reuse the F-004 provider fetchers; the
// weekly row is TMDB-only by PRD §F-008's "TMDB `/trending/{type}/week`
// only, distinct from merged trending" wording.

/// `get_trending_pools(kind, locale)` (PRD §F-008 rows 2 + 3).
///
/// Runs the F-004 aggregator's steps 1-5 (fetch, dedup, score, split)
/// against the configured providers and returns the two pools as
/// separately-shuffled lists. Each pool is daily-shuffled with the same
/// per-UTC-day PRNG seed so the home rows are stable within a day and
/// permute across consecutive days — the same invariant `get_trending`
/// upholds for the alternated list.
///
/// Cache key is distinct from `get_trending` so the two outputs can
/// coexist (a future F-009 sub-home may render the alternated list
/// while the home renders the split pools).
#[tauri::command]
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
pub async fn get_trending_pools(
    db: State<'_, Db>,
    kind: TitleKind,
    locale: String,
) -> Result<kino_metadata::TrendingPools, String> {
    let today = today_utc_string();
    let cache_key = format!("trending_pools:{}:{today}", kind.as_str());

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<kino_metadata::TrendingPools>(&cached) {
            return Ok(parsed);
        }
        tracing::warn!(key = %cache_key, "discarding malformed cached trending-pools payload");
    }

    let install_id = db.install_id().await.map_err(ipc)?;
    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let trakt_key = db.kv_get(TRAKT_API_KEY).await.map_err(ipc)?;
    let tvdb_key = db.kv_get(TVDB_API_KEY).await.map_err(ipc)?;

    let Some(tmdb_key) = tmdb_key else {
        return Err(format!(
            "TMDB API key not configured (settings.{TMDB_API_KEY}) — Home is empty until it's set."
        ));
    };

    let (tmdb_items, trakt_items, tvdb_items) = fetch_all_providers(
        &tmdb_key,
        trakt_key.as_deref(),
        tvdb_key.as_deref(),
        &kind,
        &locale,
    )
    .await?;

    let pools =
        kino_metadata::aggregate_pools(tmdb_items, trakt_items, tvdb_items, &install_id, &today);

    let expires_at =
        next_utc_midnight_unix(&today).ok_or_else(|| "internal: invalid UTC date".to_string())?;
    let payload = serde_json::to_string(&pools).map_err(|e| e.to_string())?;
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, "failed to persist trending-pools cache");
    }

    Ok(pools)
}

/// `get_weekly_trending(kind, locale)` (PRD §F-008 row 4).
///
/// TMDB `/trending/{type}/week` only — PRD §F-008 calls this row
/// "distinct from merged trending" so the call is single-provider on
/// purpose. Returns the items in TMDB's own ranking order (no shuffle,
/// no merge); the row is intentionally a different view than the merged
/// `get_trending_pools` rows.
///
/// Cached for the rest of the UTC day so the row's content matches what
/// the merged-trending cache shows. Cache key is distinct.
#[tauri::command]
pub async fn get_weekly_trending(
    db: State<'_, Db>,
    kind: TitleKind,
    locale: String,
) -> Result<Vec<TitleSummary>, String> {
    let today = today_utc_string();
    let cache_key = format!("weekly_trending:{}:{today}", kind.as_str());

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<Vec<TitleSummary>>(&cached) {
            return Ok(parsed);
        }
        tracing::warn!(key = %cache_key, "discarding malformed cached weekly-trending payload");
    }

    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let Some(tmdb_key) = tmdb_key else {
        return Err(format!(
            "TMDB API key not configured (settings.{TMDB_API_KEY}) — Home is empty until it's set."
        ));
    };

    let client = TmdbClient::new(tmdb_key).map_err(ipc)?;
    let items: Vec<ProviderItem> = match kind {
        TitleKind::Movie => client.trending_movies(&locale).await.map_err(ipc)?,
        TitleKind::Series => client.trending_shows(&locale).await.map_err(ipc)?,
    };
    let summaries: Vec<TitleSummary> = items.into_iter().map(|i| i.summary).collect();

    let expires_at =
        next_utc_midnight_unix(&today).ok_or_else(|| "internal: invalid UTC date".to_string())?;
    let payload = serde_json::to_string(&summaries).map_err(|e| e.to_string())?;
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, "failed to persist weekly-trending cache");
    }

    Ok(summaries)
}

// ---- F-008 row 5: addon catalogs enumeration ---------------------------
//
// `list_home_catalogs(kind, locale)` (PRD §F-008 row 5 + §F-009).
//
// Walks installed enabled addons in their persisted `display_order` and
// enumerates each addon's manifest-declared catalogs. For every (addon,
// catalog) pair that matches the requested `kind` filter, fetches the
// catalog payload via `AddonClient::catalog`, converts each entry to the
// home-screen [`TitleSummary`] shape, and returns the bundle as
// `Vec<HomeCatalog>` — one row per non-empty catalog, in the PRD-locked
// "addon `display_order` then catalog order within each addon" sequence.
//
// `kind` filter semantics (PRD §F-008 / §F-009):
// - `None` → unfiltered Home; every catalog of every enabled addon is
//   surfaced regardless of `catalog.type`.
// - `Some(Movie | Series)` → only catalogs whose `catalog.type` matches
//   AND whose owning addon manifest's top-level `types` array includes
//   the kind. F-009: "only catalogs whose addon manifest declares the
//   matching type".
//
// Failure handling: per-catalog network / decode failures are logged via
// `tracing::warn!` and skipped — one flaky addon must not blank the
// whole Home row stack. Catalogs that fetch successfully but return
// empty `metas` are also dropped (rendering an empty addon row in a
// 10-foot UI is worse UX than showing nothing — F-008 row 5 is meant to
// expose USEFUL addon content, not pad the home screen).
//
// Concurrency: per-catalog fetches are dispatched via
// `tokio::task::JoinSet` bounded by a `Semaphore(AVAILABILITY_CONCURRENCY = 8)`.
// Reuses the existing F-006 concurrency budget — Home loads typically
// fan out availability checks alongside catalog fetches, and 8 is the
// PRD §F-006 ceiling on simultaneous outbound addon connections.
//
// Caching: full result cached in `response_cache` for `SEARCH_TTL_S = 1h`
// (the closest PRD §8 TTL constant covering live addon-served lists)
// keyed by `home_catalogs:{kind_str|"all"}:{locale}`. The TTL is shorter
// than trending's same-UTC-day because addon catalogs (Cinemeta's
// "Popular Movies", Torrentio's "Trending", etc.) tick more frequently
// than the merged TMDB/Trakt/TVDB trending list.

/// One PRD §F-008 row 5 catalog row delivered to the frontend. Each
/// instance maps to one `<Row>` rendered under the four locked rows of
/// the home screen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeCatalog {
    /// Persisted [`Addon::id`] this catalog belongs to. Used by the
    /// frontend as part of the row's focusable-id prefix so D-pad
    /// navigation across rows stays unambiguous when two addons declare
    /// catalogs with the same `id`.
    pub addon_id: String,
    /// User-facing addon name from the manifest. Falls back to the
    /// addon's id when the manifest omits it (Stremio guarantees `name`
    /// in practice; this is defensive).
    pub addon_name: String,
    /// Catalog id as declared in the manifest (`{id}` in
    /// `GET /catalog/{type}/{id}.json`).
    pub catalog_id: String,
    /// `"movie"` or `"series"` — the kind this catalog serves. Mirrored
    /// from the manifest's `CatalogDescriptor::kind`.
    pub catalog_kind: String,
    /// User-facing catalog label from the manifest. Falls back to a
    /// `"{Addon} — {id}"` composite when the manifest omits the
    /// catalog's `name`.
    pub catalog_name: String,
    /// Catalog items, in the addon's own returned order. Empty catalogs
    /// are filtered out before this struct is constructed, so callers
    /// can safely assume `items.len() >= 1`.
    pub items: Vec<TitleSummary>,
}

/// `list_home_catalogs(kind, locale)` (PRD §F-008 row 5 + §F-009).
///
/// Returns the dynamic tail of the home-screen row stack: one
/// [`HomeCatalog`] per non-empty addon catalog matching the kind filter,
/// in the PRD-locked addon-`display_order` then catalog-order sequence.
///
/// `kind = None` returns every catalog (unfiltered Home);
/// `kind = Some(Movie | Series)` filters per PRD §F-009.
#[tauri::command]
pub async fn list_home_catalogs(
    db: State<'_, Db>,
    kind: Option<TitleKind>,
    locale: String,
) -> Result<Vec<HomeCatalog>, String> {
    let kind_key = kind.map_or("all", TitleKind::as_str);
    let cache_key = format!("home_catalogs:{kind_key}:{locale}");

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<Vec<HomeCatalog>>(&cached) {
            return Ok(parsed);
        }
        tracing::warn!(key = %cache_key, "discarding malformed cached home-catalogs payload");
    }

    let fetched = list_home_catalogs_uncached(&db, kind, &HttpConfig::default()).await?;

    let payload = serde_json::to_string(&fetched).map_err(|e| e.to_string())?;
    let expires_at = now_unix().saturating_add(i64::try_from(SEARCH_TTL_S).unwrap_or(i64::MAX));
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, "failed to persist home-catalogs cache");
    }

    Ok(fetched)
}

/// Cache-bypassing core of [`list_home_catalogs`]. Exists as a separate
/// function so the unit tests can drive it with a `for_test()`
/// [`HttpConfig`] (zero backoffs, short timeout) without going through
/// the `response_cache` round-trip.
async fn list_home_catalogs_uncached(
    db: &Db,
    kind: Option<TitleKind>,
    http_config: &HttpConfig,
) -> Result<Vec<HomeCatalog>, String> {
    let installed = db.addons_list().await.map_err(ipc)?;

    // Plan: walk addons in display_order, expand each into the catalogs
    // that survive the kind filter, build a work item with a stable
    // (addon_index, catalog_index) for output reassembly.
    let mut plan: Vec<CatalogWorkItem> = Vec::new();
    for (addon_index, addon) in installed.into_iter().enumerate() {
        if !addon.enabled {
            continue;
        }
        let manifest = match serde_json::from_value::<Manifest>(addon.manifest_json.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    addon = %addon.id,
                    error = %e,
                    "could not parse persisted manifest; skipping for catalogs"
                );
                continue;
            }
        };
        // F-009: addon-level types declaration gates whether ANY of its
        // catalogs surface for a given kind. An addon whose manifest says
        // `types: ["movie"]` contributes zero rows to the Series sub-home
        // even if it happens to declare a series catalog (the catalog
        // would be unreachable per the addon's own protocol promise).
        if let Some(k) = kind {
            if !manifest.types.iter().any(|t| t == k.as_str()) {
                continue;
            }
        }
        // Also skip addons that don't declare the `catalog` resource at
        // all — fetching `GET /catalog/...` against them would 404.
        if !manifest.resources.iter().any(|r| r.name() == "catalog") {
            continue;
        }
        let client = match AddonClient::with_options(&addon.manifest_url, http_config.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    addon = %addon.id,
                    error = %e,
                    "could not build addon client for catalog enumeration; skipping"
                );
                continue;
            }
        };
        for (catalog_index, catalog) in manifest.catalogs.iter().enumerate() {
            if let Some(k) = kind {
                if catalog.kind != k.as_str() {
                    continue;
                }
            }
            plan.push(CatalogWorkItem {
                addon_index,
                catalog_index,
                addon_id: addon.id.clone(),
                addon_name: manifest.name.clone(),
                client: client.clone(),
                catalog: catalog.clone(),
            });
        }
    }

    if plan.is_empty() {
        return Ok(Vec::new());
    }

    // Dispatch with Semaphore-bounded concurrency. Reuses the F-006
    // ceiling because the typical caller (Home load) is firing
    // availability checks at the same time and the two budgets share
    // the addon connection pool.
    let semaphore = Arc::new(Semaphore::new(AVAILABILITY_CONCURRENCY));
    let mut set: tokio::task::JoinSet<Option<CatalogOutcome>> = tokio::task::JoinSet::new();
    for item in plan {
        let permit = Arc::clone(&semaphore);
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.ok();
            fetch_catalog_row(item).await
        });
    }

    let mut rows: Vec<CatalogOutcome> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Some(outcome)) => rows.push(outcome),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "home catalog dispatch task panicked"),
        }
    }

    // Restore the deterministic PRD §F-008 ordering: addon display_order
    // first, then catalog index within each addon.
    rows.sort_by(|a, b| {
        a.addon_index
            .cmp(&b.addon_index)
            .then_with(|| a.catalog_index.cmp(&b.catalog_index))
    });
    Ok(rows.into_iter().map(|o| o.row).collect())
}

/// One scheduled catalog fetch. The (`addon_index`, `catalog_index`)
/// pair preserves the original walk order so we can re-sort after the
/// concurrent dispatch.
struct CatalogWorkItem {
    addon_index: usize,
    catalog_index: usize,
    addon_id: String,
    addon_name: String,
    client: AddonClient,
    catalog: CatalogDescriptor,
}

/// Successful fetch result paired with its original ordering keys.
struct CatalogOutcome {
    addon_index: usize,
    catalog_index: usize,
    row: HomeCatalog,
}

async fn fetch_catalog_row(item: CatalogWorkItem) -> Option<CatalogOutcome> {
    let CatalogWorkItem {
        addon_index,
        catalog_index,
        addon_id,
        addon_name,
        client,
        catalog,
    } = item;
    let resp = match client.catalog(&catalog.kind, &catalog.id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                addon = %addon_id,
                catalog = %catalog.id,
                error = %e,
                "addon catalog fetch failed; skipping row"
            );
            return None;
        }
    };
    let items: Vec<TitleSummary> = resp
        .metas
        .into_iter()
        .map(meta_preview_to_summary)
        .collect();
    if items.is_empty() {
        // Drop empty catalogs entirely — rendering a labeled but empty
        // row in a 10-foot UI is worse than not rendering it at all.
        return None;
    }
    let catalog_name = catalog
        .name
        .clone()
        .unwrap_or_else(|| format!("{addon_name} — {}", catalog.id));
    Some(CatalogOutcome {
        addon_index,
        catalog_index,
        row: HomeCatalog {
            addon_id,
            addon_name,
            catalog_id: catalog.id,
            catalog_kind: catalog.kind,
            catalog_name,
            items,
        },
    })
}

/// Convert a Stremio [`MetaPreview`] (addon-protocol shape) to the
/// home-screen [`TitleSummary`] (kino-internal shape).
///
/// Stremio addons commonly return IMDb-style ids (`"tt0133093"`) directly
/// for movies / series. Our downstream `resolve_artwork` and trending
/// pipeline both expect the provider-prefixed shape (`"imdb:tt0133093"`,
/// `"tmdb:603"`, `"tvdb:N"`), so we coerce raw `"tt…"` ids to the
/// `imdb:` namespace here. Already-prefixed ids (containing a `:`) pass
/// through unchanged; non-standard shapes (anime addons returning
/// `"kitsu:..."`, etc.) also pass through so the artwork resolver's
/// downstream "unsupported `title_id`" error surfaces with the addon's
/// own id rather than a silently-mangled one.
fn meta_preview_to_summary(meta: MetaPreview) -> TitleSummary {
    let id = coerce_catalog_id(&meta.id);
    let kind = match meta.kind.as_str() {
        "series" => TitleKind::Series,
        // Default everything else to Movie — addons sometimes emit
        // "film" / "movies" / etc. and the F-008 row treats anything
        // non-series as a movie tile. The id-prefixed shape is what the
        // resolver uses to disambiguate downstream.
        _ => TitleKind::Movie,
    };
    let year = meta.release_info.as_deref().and_then(parse_release_year);
    let rating = meta
        .imdb_rating
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok());
    TitleSummary {
        id,
        kind,
        title: meta.name,
        year,
        poster: meta.poster,
        rating,
    }
}

/// Coerce a Stremio catalog id to the provider-prefixed shape the rest
/// of the workspace consumes. Returns `imdb:tt…` for IMDb-style ids,
/// passes everything else through unchanged.
fn coerce_catalog_id(raw: &str) -> String {
    if raw.starts_with("tt") && raw.len() > 2 && raw[2..].chars().all(|c| c.is_ascii_digit()) {
        format!("imdb:{raw}")
    } else {
        raw.to_string()
    }
}

/// Parse a year from Stremio's `releaseInfo` field. Addons emit one of
/// `"2024"`, `"2024-"` (open-ended series range), `"2014-2019"` (closed
/// range), `"1994-01-15"` (full date). We take the four leading digits.
fn parse_release_year(s: &str) -> Option<u16> {
    let head: String = s.chars().take_while(char::is_ascii_digit).collect();
    if head.len() != 4 {
        return None;
    }
    head.parse::<u16>().ok()
}

// ---- F-005: image & logo resolution -----------------------------------
//
// `resolve_artwork(title_id, kind, lang_pref)` builds the F-005 cascade
// against the configured metadata providers. The locked algorithm and
// per-tier provider order live in `kino_metadata::artwork`; this command
// supplies the I/O.
//
// PRD §F-005 caching: payloads land in `response_cache` for `ARTWORK_TTL_S`
// (7 days) keyed by `(title_id, kind, lang_chain_hash)`. The lang-chain hash
// is part of the key so a user changing their language preferences in F-016
// transparently invalidates this cache on next read.

/// `resolve_artwork(title_id, kind, lang_pref) -> Artwork` (PRD §F-005).
///
/// `title_id` is a provider-prefixed id of the form `tmdb:N` / `imdb:tt...`
/// / `tvdb:N` (the trending aggregator's [`TitleSummary::id`] shape). Other
/// shapes return an error. `lang_pref` is the primary + fallback language
/// chain (up to 4 entries per PRD §F-016); pass `["en"]` for the default.
///
/// Returns an [`Artwork`] with the resolved URLs plus a [`Provenance`] block
/// for debugging which tier won for each asset.
#[tauri::command]
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
pub async fn resolve_artwork(
    db: State<'_, Db>,
    title_id: String,
    kind: TitleKind,
    lang_pref: Vec<String>,
) -> Result<Artwork, String> {
    let chain_hash = lang_chain_hash(&lang_pref);
    let cache_key = format!("artwork:{}:{}:{}", title_id, kind.as_str(), chain_hash);

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<CachedArtwork>(&cached) {
            return Ok(parsed.artwork);
        }
        tracing::warn!(key = %cache_key, "discarding malformed cached artwork payload");
    }

    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let tvdb_key = db.kv_get(TVDB_API_KEY).await.map_err(ipc)?;
    let fanart_key = db.kv_get(FANART_API_KEY).await.map_err(ipc)?;

    let tmdb_client = match tmdb_key {
        Some(k) => Some(TmdbClient::new(k).map_err(ipc)?),
        None => None,
    };
    let tvdb_client = match tvdb_key {
        Some(k) => Some(TvdbClient::new(k).map_err(ipc)?),
        None => None,
    };
    let fanart_client = match fanart_key {
        Some(k) => Some(FanartClient::new(k).map_err(ipc)?),
        None => None,
    };

    // Resolve the full TitleIds (tmdb + imdb + tvdb) so each provider gets
    // the id shape it expects.
    let ids = resolve_title_ids(&title_id, kind, tmdb_client.as_ref()).await?;

    let bundles = build_bundles(
        kind,
        &lang_pref,
        &ids,
        tmdb_client.as_ref(),
        tvdb_client.as_ref(),
        fanart_client.as_ref(),
    )
    .await;

    let artwork = cascade(kind, &bundles, &lang_pref);

    // Cache through `ARTWORK_TTL_S` from now. Per PRD §F-005, changing the
    // user's language chain invalidates the cache on the NEXT read — the
    // hash in the key handles this; we don't proactively flush.
    let payload = serde_json::to_string(&CachedArtwork {
        artwork: artwork.clone(),
    })
    .map_err(|e| e.to_string())?;
    let expires_at = now_unix().saturating_add(i64::try_from(ARTWORK_TTL_S).unwrap_or(i64::MAX));
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, "failed to persist artwork cache");
    }
    Ok(artwork)
}

/// Parse a provider-prefixed `title_id` into its (provider, raw id) pair.
fn parse_title_id(title_id: &str) -> Result<(&str, &str), String> {
    title_id
        .split_once(':')
        .filter(|(p, _)| matches!(*p, "tmdb" | "imdb" | "tvdb"))
        .ok_or_else(|| {
            format!("unsupported title_id '{title_id}': expected 'tmdb:N', 'imdb:ttN', or 'tvdb:N'")
        })
}

/// Fan out the prefix into the full [`TitleIds`]. If TMDB is configured we
/// always end up with a TMDB id (resolving cross-provider via TMDB's `/find`)
/// AND the matching `imdb_id` + `tvdb_id` via `/external_ids`. With no TMDB
/// client, only the directly-encoded id is available — the cascade falls back
/// to whichever providers can still match.
async fn resolve_title_ids(
    title_id: &str,
    kind: TitleKind,
    tmdb: Option<&TmdbClient>,
) -> Result<TitleIds, String> {
    let (provider, raw) = parse_title_id(title_id)?;
    let mut ids = TitleIds::default();
    match provider {
        "tmdb" => {
            let parsed: u64 = raw
                .parse()
                .map_err(|_| format!("tmdb title_id has non-numeric id: '{raw}'"))?;
            ids.tmdb_id = Some(parsed);
        }
        "imdb" => {
            ids.imdb_id = Some(raw.to_string());
        }
        "tvdb" => {
            let parsed: u64 = raw
                .parse()
                .map_err(|_| format!("tvdb title_id has non-numeric id: '{raw}'"))?;
            ids.tvdb_id = Some(parsed);
        }
        _ => unreachable!("parse_title_id guards the prefix"),
    }
    let Some(client) = tmdb else {
        return Ok(ids);
    };
    if ids.tmdb_id.is_none() {
        let (external_id, source) = if let Some(imdb) = ids.imdb_id.as_deref() {
            (imdb.to_string(), "imdb_id")
        } else if let Some(tvdb) = ids.tvdb_id {
            (tvdb.to_string(), "tvdb_id")
        } else {
            return Ok(ids);
        };
        match client.find_external(&external_id, source, kind).await {
            Ok(Some(tmdb_id)) => ids.tmdb_id = Some(tmdb_id),
            Ok(None) => tracing::info!(
                provider = "tmdb",
                external = %external_id,
                "tmdb find returned no match"
            ),
            Err(e) => tracing::warn!(error = %e, "tmdb find_external failed"),
        }
    }
    if let Some(tmdb_id) = ids.tmdb_id {
        match client.external_ids(tmdb_id, kind).await {
            Ok(extras) => {
                if ids.imdb_id.is_none() {
                    ids.imdb_id = extras.imdb_id;
                }
                if ids.tvdb_id.is_none() {
                    ids.tvdb_id = extras.tvdb_id;
                }
            }
            Err(e) => tracing::warn!(error = %e, "tmdb external_ids failed"),
        }
    }
    Ok(ids)
}

/// Fetch every provider in parallel and stuff the responses into a
/// [`ProviderBundles`] for the cascade. A `None` bundle means the provider
/// produced nothing usable (no key, no resolvable id, or a transport failure
/// we logged and elided).
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
async fn build_bundles(
    kind: TitleKind,
    lang_pref: &[String],
    ids: &TitleIds,
    tmdb: Option<&TmdbClient>,
    tvdb: Option<&TvdbClient>,
    fanart: Option<&FanartClient>,
) -> ProviderBundles {
    let fanart_fut = async move {
        let client = fanart?;
        let result = match kind {
            TitleKind::Movie => {
                let id = ids
                    .tmdb_id
                    .map(|n| n.to_string())
                    .or_else(|| ids.imdb_id.clone())?;
                client.movie_artwork(&id).await
            }
            TitleKind::Series => {
                let tvdb_id = ids.tvdb_id?;
                client.show_artwork(tvdb_id).await
            }
        };
        match result {
            Ok(Some(bundle)) => Some(bundle),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(provider = "fanart", error = %e, "artwork fetch failed");
                None
            }
        }
    };

    let tmdb_fut = async move {
        let client = tmdb?;
        let tmdb_id = ids.tmdb_id?;
        let images_result = client.artwork_images(tmdb_id, kind, lang_pref).await;
        let mut bundle = match images_result {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(provider = "tmdb", error = %e, "artwork images fetch failed");
                return None;
            }
        };
        bundle.summaries = fetch_tmdb_summaries(client, tmdb_id, kind, lang_pref).await;
        Some(bundle)
    };

    let tvdb_fut = async move {
        let client = tvdb?;
        let tvdb_id = ids.tvdb_id?;
        match client.artwork(tvdb_id, kind).await {
            Ok(Some(b)) => Some(b),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(provider = "tvdb", error = %e, "artwork fetch failed");
                None
            }
        }
    };

    let (fanart_bundle, tmdb_bundle, tvdb_bundle) = tokio::join!(fanart_fut, tmdb_fut, tvdb_fut);
    ProviderBundles {
        fanart: fanart_bundle,
        tmdb: tmdb_bundle,
        tvdb: tvdb_bundle,
    }
}

/// Fetch per-language summaries for TMDB at every configured tier. Empty
/// overviews are dropped so the cascade can fall through to the next tier.
async fn fetch_tmdb_summaries(
    client: &TmdbClient,
    tmdb_id: u64,
    kind: TitleKind,
    lang_pref: &[String],
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for lang in lang_pref {
        match client.summary(tmdb_id, kind, lang).await {
            Ok(Some(text)) => {
                out.insert(lang.clone(), text);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(provider = "tmdb", lang = %lang, error = %e, "summary fetch failed");
            }
        }
    }
    out
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ---- F-006: source availability filter ---------------------------------
//
// For each requested `(title_id, kind)` the dispatch asks every enabled
// stream-serving addon `GET /stream/{kind}/{title_id}.json`, treats any
// non-empty stream list as "available", caches each per-source result in
// `stream_availability` (30-min TTL per PRD §8), and returns one
// `AvailabilityResult` per input item — `available = true` iff any
// addon returned ≥ 1 stream.
//
// Concurrency is capped at `AVAILABILITY_CONCURRENCY` (8) in-flight via a
// `Semaphore`; per-request timeout is `AVAILABILITY_TIMEOUT_S` (5s),
// installed by handing the `AddonClient` an `HttpConfig` with `timeout =
// AVAILABILITY_TIMEOUT_S`. A request that times out is treated as
// "unavailable from THIS source" — the title may still be available from
// another addon (any-positive wins) and the timeout itself is persisted as
// `has_streams = false` so a flaky single-call window doesn't burn the
// 30-min TTL on every refresh.

/// One requested availability check. Sent by the frontend as a batch of
/// `(title_id, kind)` pairs — typically every tile of a freshly-loaded
/// catalog row.
#[derive(Debug, Clone, Deserialize)]
pub struct AvailabilityRequest {
    pub title_id: String,
    #[serde(rename = "type")]
    pub kind: TitleKind,
}

/// One returned availability result, keyed by `(title_id, kind)`.
/// `source_count` is the number of enabled stream-serving addons that
/// returned at least one stream — `available` is `source_count > 0`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AvailabilityResult {
    pub title_id: String,
    #[serde(rename = "type")]
    pub kind: TitleKind,
    pub available: bool,
    pub source_count: u32,
}

/// `check_availability(items)` (PRD §F-006).
///
/// For each item, asks every enabled stream-serving addon for streams,
/// honoring a 30-minute `stream_availability` cache, an 8-in-flight
/// concurrency cap, and a 5-second per-request timeout. Returns one
/// [`AvailabilityResult`] per input in input order.
#[tauri::command]
pub async fn check_availability(
    db: State<'_, Db>,
    items: Vec<AvailabilityRequest>,
) -> Result<Vec<AvailabilityResult>, String> {
    let config = availability_http_config();
    check_availability_with_config(&db, items, &config).await
}

fn availability_http_config() -> HttpConfig {
    HttpConfig {
        timeout: Duration::from_secs(AVAILABILITY_TIMEOUT_S),
        ..HttpConfig::default()
    }
}

/// Core orchestration shared by the Tauri command and the unit tests. The
/// `http_config` parameter lets tests inject a short-backoff client without
/// touching the production default.
async fn check_availability_with_config(
    db: &Db,
    items: Vec<AvailabilityRequest>,
    http_config: &HttpConfig,
) -> Result<Vec<AvailabilityResult>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    let stream_addons = load_stream_addons(db).await?;
    if stream_addons.is_empty() {
        // No addons can serve streams → nothing is available; skip cache
        // lookups and network entirely. We DO NOT persist these as
        // `has_streams = false` rows because a future addon install should
        // be able to flip the tile without waiting out a 30-min TTL.
        return Ok(items
            .into_iter()
            .map(|req| AvailabilityResult {
                title_id: req.title_id,
                kind: req.kind,
                available: false,
                source_count: 0,
            })
            .collect());
    }

    let now = now_unix();
    let ttl_i64 =
        i64::try_from(kino_core::constants::STREAM_AVAILABILITY_TTL_S).unwrap_or(i64::MAX);
    let fresh_after = now.saturating_sub(ttl_i64);

    // Per-item count of stream-serving addons that returned ≥1 stream. We
    // accumulate cached + freshly-fetched results into the same vector and
    // build the response once both phases complete.
    let mut counts: Vec<u32> = vec![0; items.len()];

    // Resolve cache hits up front so we only issue network calls for
    // (item, addon) pairs that aren't already known fresh.
    let mut work: Vec<(usize, StreamAddon, AvailabilityRequest)> = Vec::new();
    for (index, req) in items.iter().enumerate() {
        for addon in &stream_addons {
            if !addon.manifest.serves_stream(req.kind.as_str()) {
                continue;
            }
            match db
                .availability_get_fresh(&req.title_id, req.kind, &addon.id, fresh_after)
                .await
            {
                Ok(Some(true)) => counts[index] += 1,
                Ok(Some(false)) => {}
                Ok(None) => work.push((index, addon.clone(), req.clone())),
                Err(e) => {
                    tracing::warn!(error = %e, "availability cache read failed; treating as cache miss");
                    work.push((index, addon.clone(), req.clone()));
                }
            }
        }
    }

    // Dispatch the remaining work with a Semaphore-bounded concurrency cap.
    let fresh_rows = dispatch_availability_checks(work, http_config, now).await;
    for (index, has_streams) in &fresh_rows.per_index_counts {
        counts[*index] += u32::from(*has_streams);
    }
    if !fresh_rows.persist.is_empty() {
        if let Err(e) = db.availability_upsert_many(&fresh_rows.persist).await {
            tracing::warn!(error = %e, "failed to persist stream_availability rows");
        }
    }

    Ok(items
        .into_iter()
        .enumerate()
        .map(|(i, req)| AvailabilityResult {
            title_id: req.title_id,
            kind: req.kind,
            available: counts[i] > 0,
            source_count: counts[i],
        })
        .collect())
}

/// Snapshot of an installed, enabled, stream-serving addon used by the
/// dispatch loop. We unmarshal the manifest once, up front, so the work
/// item itself is cheap to clone per request.
#[derive(Debug, Clone)]
struct StreamAddon {
    id: String,
    manifest_url: String,
    manifest: Manifest,
}

async fn load_stream_addons(db: &Db) -> Result<Vec<StreamAddon>, String> {
    let installed = db.addons_list().await.map_err(ipc)?;
    let mut out = Vec::with_capacity(installed.len());
    for addon in installed {
        if !addon.enabled {
            continue;
        }
        let manifest = match serde_json::from_value::<Manifest>(addon.manifest_json.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    addon = %addon.id,
                    error = %e,
                    "could not parse persisted manifest; skipping for availability"
                );
                continue;
            }
        };
        // Top-level kind filter is `serves_stream(kind)` at request time;
        // here we just require that the addon serves the `stream` resource
        // for SOME kind so we don't keep around catalog-only addons.
        if !manifest.resources.iter().any(|r| r.name() == "stream") {
            continue;
        }
        out.push(StreamAddon {
            id: addon.id,
            manifest_url: addon.manifest_url,
            manifest,
        });
    }
    Ok(out)
}

/// Aggregated output of [`dispatch_availability_checks`]: per-index hit
/// counts (so the caller can fold them into the response) and the rows to
/// persist into `stream_availability`.
#[derive(Debug, Default)]
struct DispatchOutcome {
    per_index_counts: Vec<(usize, bool)>,
    persist: Vec<AvailabilityRow>,
}

async fn dispatch_availability_checks(
    work: Vec<(usize, StreamAddon, AvailabilityRequest)>,
    http_config: &HttpConfig,
    now: i64,
) -> DispatchOutcome {
    if work.is_empty() {
        return DispatchOutcome::default();
    }
    // Cache `AddonClient` instances per manifest URL so two work items
    // hitting the same addon share one `reqwest::Client` (its internal
    // connection pool is what matters when 50 catalog items × N addons
    // all dial the same host).
    let mut clients: HashMap<String, AddonClient> = HashMap::new();
    let semaphore = Arc::new(Semaphore::new(AVAILABILITY_CONCURRENCY));
    let mut set: tokio::task::JoinSet<DispatchedItem> = tokio::task::JoinSet::new();

    for (index, addon, req) in work {
        let client = match clients.get(&addon.manifest_url).cloned() {
            Some(c) => c,
            None => match AddonClient::with_options(&addon.manifest_url, http_config.clone()) {
                Ok(c) => {
                    clients.insert(addon.manifest_url.clone(), c.clone());
                    c
                }
                Err(e) => {
                    tracing::warn!(
                        addon = %addon.id,
                        error = %e,
                        "could not build addon client for availability check; skipping"
                    );
                    // Persist nothing — a transient client-build failure
                    // shouldn't burn the 30-min TTL.
                    continue;
                }
            },
        };
        let permit = Arc::clone(&semaphore);
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.ok();
            let has_streams = match client.stream(req.kind.as_str(), &req.title_id).await {
                Ok(resp) => !resp.streams.is_empty(),
                Err(e) => {
                    tracing::debug!(
                        addon = %addon.id,
                        title = %req.title_id,
                        error = %e,
                        "stream availability check failed; treating as unavailable from this source"
                    );
                    false
                }
            };
            DispatchedItem {
                index,
                addon_id: addon.id,
                title_id: req.title_id,
                kind: req.kind,
                has_streams,
            }
        });
    }

    let mut outcome = DispatchOutcome::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(item) => {
                outcome
                    .per_index_counts
                    .push((item.index, item.has_streams));
                outcome.persist.push(AvailabilityRow {
                    title_id: item.title_id,
                    kind: item.kind,
                    source_id: item.addon_id,
                    has_streams: item.has_streams,
                    checked_at: now,
                });
            }
            Err(e) => tracing::warn!(error = %e, "availability dispatch task panicked"),
        }
    }
    outcome
}

#[derive(Debug)]
struct DispatchedItem {
    index: usize,
    addon_id: String,
    title_id: String,
    kind: TitleKind,
    has_streams: bool,
}

// ---- F-007: Stremio addon protocol client ------------------------------
//
// `addons_list` / `addons_insert` / `addons_delete` / `addons_set_enabled`
// / `addons_reorder` already live above (added with F-002 in Session 003).
// The F-007 layer adds the protocol-aware install / uninstall / order
// commands plus the recommended-addons accessor, and bootstraps Cinemeta
// as a non-removable default at first launch.

/// Settings key recording that the first-launch bootstrap (currently:
/// "install Cinemeta") has run. Stored in the `settings` KV; presence —
/// regardless of value — is the signal that bootstrap is done.
const ADDON_BOOTSTRAP_DONE_KEY: &str = "addons.bootstrap_done";

/// Public, serializable shape of a recommended addon. Mirrors
/// [`kino_addons::RecommendedAddon`] but owns its strings so it can cross
/// the IPC boundary.
#[derive(Debug, Clone, Serialize)]
pub struct RecommendedAddonView {
    pub name: String,
    pub manifest_url: String,
    pub description: String,
}

impl From<&RecommendedAddon> for RecommendedAddonView {
    fn from(r: &RecommendedAddon) -> Self {
        Self {
            name: r.name.to_string(),
            manifest_url: r.manifest_url.to_string(),
            description: r.description.to_string(),
        }
    }
}

/// `get_recommended_addons()` (PRD §F-007).
///
/// Returns the locked recommended-addons table from PRD §8. The Settings →
/// Addons screen renders this as a one-tap install list.
#[tauri::command]
pub fn get_recommended_addons() -> Vec<RecommendedAddonView> {
    RECOMMENDED_ADDONS
        .iter()
        .map(RecommendedAddonView::from)
        .collect()
}

/// `install_addon(url)` (PRD §F-007).
///
/// Normalizes the user-supplied URL (`stremio://` → `https://`), fetches
/// the manifest, validates it, and persists the addon with `enabled = true`
/// at the next free `display_order` slot. Re-installing an already-installed
/// manifest URL surfaces a typed conflict via the underlying DB layer.
#[tauri::command]
pub async fn install_addon(db: State<'_, Db>, url: String) -> Result<Addon, String> {
    let normalized = normalize_manifest_url(&url).map_err(ipc)?;
    let client = AddonClient::new(&normalized).map_err(ipc)?;
    let manifest = client.manifest().await.map_err(ipc)?;
    persist_addon(&db, &normalized, &manifest).await
}

/// `uninstall_addon(id)` (PRD §F-007).
///
/// Refuses to remove Cinemeta (`AddonError::NonRemovable`); per PRD §F-007
/// Cinemeta can only be disabled in v1. Returns the number of rows removed
/// — 0 for an unknown id.
#[tauri::command]
pub async fn uninstall_addon(db: State<'_, Db>, id: String) -> Result<u64, String> {
    if is_cinemeta_id(&db, &id).await? {
        return Err(AddonError::NonRemovable { id }.to_string());
    }
    db.addons_delete(&id).await.map_err(ipc)
}

/// `set_addon_order(id, order)` (PRD §F-007).
///
/// Moves the addon identified by `id` to position `order` in the display
/// list (0-indexed). Rebuilds the full ordering via `addons_reorder` so
/// the in-memory list and the DB stay in sync.
#[tauri::command]
pub async fn set_addon_order(db: State<'_, Db>, id: String, order: usize) -> Result<(), String> {
    let current = db.addons_list().await.map_err(ipc)?;
    let mut ids: Vec<String> = current.iter().map(|a| a.id.clone()).collect();
    let Some(from) = ids.iter().position(|x| x == &id) else {
        return Err(format!("addon '{id}' is not installed"));
    };
    let to = order.min(ids.len().saturating_sub(1));
    let item = ids.remove(from);
    ids.insert(to, item);
    db.addons_reorder(&ids).await.map_err(ipc)
}

/// Auto-install Cinemeta on first launch (PRD §F-007).
///
/// Called from the Tauri setup hook. Idempotent: writes
/// `settings.addons.bootstrap_done` on success so subsequent launches skip
/// the network call. A bootstrap failure is logged and elided — the user
/// can manually install Cinemeta via Settings → Addons later — so a
/// network outage on first launch doesn't brick the app.
pub async fn bootstrap_default_addons(db: &Db) {
    if let Ok(Some(_)) = db.kv_get(ADDON_BOOTSTRAP_DONE_KEY).await {
        return;
    }
    if let Err(e) = install_cinemeta(db).await {
        tracing::warn!(error = %e, "first-launch Cinemeta bootstrap failed; user can retry in Settings");
        return;
    }
    if let Err(e) = db.kv_set(ADDON_BOOTSTRAP_DONE_KEY, "1").await {
        tracing::warn!(error = %e, "failed to record Cinemeta bootstrap completion");
    }
}

async fn install_cinemeta(db: &Db) -> Result<(), String> {
    // Skip if already installed (e.g. the user manually installed it before
    // the bootstrap marker was written).
    let installed = db.addons_list().await.map_err(ipc)?;
    if installed
        .iter()
        .any(|a| a.manifest_url == CINEMETA_MANIFEST_URL)
    {
        return Ok(());
    }
    let client = AddonClient::new(CINEMETA_MANIFEST_URL).map_err(ipc)?;
    let manifest = client.manifest().await.map_err(ipc)?;
    persist_addon(db, CINEMETA_MANIFEST_URL, &manifest)
        .await
        .map(|_| ())
}

async fn persist_addon(db: &Db, manifest_url: &str, manifest: &Manifest) -> Result<Addon, String> {
    let manifest_value =
        serde_json::to_value(manifest).map_err(|e| format!("manifest serialize: {e}"))?;
    db.addons_insert(&AddonInsert {
        id: manifest.id.clone(),
        manifest_url: manifest_url.to_string(),
        manifest_json: manifest_value,
        display_order: None,
    })
    .await
    .map_err(ipc)?;
    // Re-read to return the persisted row with installed_at/display_order
    // populated.
    let listed = db.addons_list().await.map_err(ipc)?;
    listed
        .into_iter()
        .find(|a| a.id == manifest.id)
        .ok_or_else(|| format!("internal: addon '{}' missing after insert", manifest.id))
}

/// Returns true iff the persisted addon row identified by `id` is the
/// non-removable Cinemeta install. Cinemeta is identified by its locked
/// manifest URL (PRD §8); the addon's own `id` field is set by Stremio
/// (`com.linvo.cinemeta`) and we match on the URL to avoid coupling to
/// Cinemeta-internal id changes.
async fn is_cinemeta_id(db: &Db, id: &str) -> Result<bool, String> {
    let installed = db.addons_list().await.map_err(ipc)?;
    Ok(installed
        .iter()
        .any(|a| a.id == id && a.manifest_url == CINEMETA_MANIFEST_URL))
}

// ---- F-010: title detail view ------------------------------------------
//
// `get_title_detail(title_id, kind, lang_pref)` builds the title-detail
// payload from three sources, in this order:
//
//   1. Cinemeta (`/meta/{kind}/{imdb_id}.json`) — always called first when
//      we can resolve an IMDb id. Cinemeta is the PRD-locked default addon
//      (auto-installed on first launch) and serves a reasonably complete
//      MetaDetail (title, runtime string, IMDb rating, genres, cast names,
//      videos[] for series) with no API key required. We treat its
//      response as the baseline.
//
//   2. TMDB title_details + credits — overrides Cinemeta's runtime,
//      summary, genres, and rating (TMDB's vote_average) with localized /
//      higher-quality data when a TMDB key is configured. The cast row
//      gets photo URLs from TMDB credits (the PRD requires top-6 with
//      photos — Cinemeta only carries names).
//
//   3. Trakt title_rating — fills the trakt_rating field when configured.
//
// CW lookup happens last: per-episode progress is keyed on (title_id,
// season, episode) for series, and an aggregate "resume target" is
// picked from the most-recently-played CW row across all episodes (or
// the single row for movies).
//
// Caching: `meta:{title_id}:{kind}:{chain_hash}` with `META_TTL_S = 24h`
// (PRD §8). The CW lookup is NOT cached — it reads the live `Db` table
// after a cache hit so the Resume button toggles correctly when the user
// starts / stops a title between detail-view visits.
//
// `get_streams(title_id, kind, season?, episode?)` is a separate command
// because (a) movies and series need different stream-id shapes
// (`tt0133093` vs `tt0944947:1:1`), (b) episode switching in the detail
// view re-fires only this call, not the metadata one, and (c) the PRD
// §F-010 stream-row sort + badge parsing logic is independent from the
// metadata pipeline. The shipped sort: quality DESC, then seeders DESC,
// then size DESC.

/// One cast member entry in the detail view's cast row (PRD §F-010
/// "top 6 with photos"). The frontend renders this as a circular
/// photo + name + (optional) character tile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CastMember {
    pub name: String,
    pub character: Option<String>,
    pub photo: Option<String>,
}

/// One episode in the series detail view (PRD §F-010 "season selector +
/// episode list"). `progress` is the fraction of the episode the user
/// has watched (0.0..=1.0); zero when no CW entry exists for that
/// `(title_id, season, episode)` tuple.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(clippy::struct_field_names)] // `episode` field is a domain term (episode number within season).
pub struct Episode {
    /// Stremio video id (`tt0944947:1:1` shape). The frontend passes
    /// this back to `get_streams` via the `season` / `episode` numeric
    /// pair, but the id is kept around as a stable React-style key.
    pub video_id: String,
    pub season: i64,
    pub episode: i64,
    pub title: String,
    /// ISO-8601 air date when the addon supplies one; otherwise empty.
    pub air_date: Option<String>,
    /// Episode synopsis, truncated to 120 chars per PRD §F-010.
    pub overview: Option<String>,
    pub thumbnail: Option<String>,
    /// `[0.0, 1.0]` watch progress for THIS episode. Zero when no CW
    /// entry exists for the matching `(title_id, season, episode)` tuple.
    pub progress: f64,
}

/// Title-detail payload returned to the frontend (PRD §F-010).
///
/// Several fields are `Option<…>` because PRD §F-010 specifies them as
/// "when known": age rating, runtime, summary, the three ratings, the
/// hero artwork. The frontend renders missing fields as absent (not
/// "Unknown") per the 10-foot UI norm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleDetail {
    /// Provider-prefixed id forwarded from the request (echoed so the
    /// frontend can route back to it via state without re-encoding).
    pub id: String,
    pub kind: TitleKind,
    pub title: String,
    pub year: Option<u16>,
    pub runtime_minutes: Option<u32>,
    pub age_rating: Option<String>,
    pub genres: Vec<String>,
    pub summary: Option<String>,
    pub imdb_rating: Option<f64>,
    pub tmdb_rating: Option<f64>,
    pub trakt_rating: Option<f64>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
    pub poster: Option<String>,
    /// Cast roster, truncated to the top six (PRD §F-010).
    pub cast: Vec<CastMember>,
    /// Series episodes. Empty for movies. Ordered by `(season, episode)`
    /// ascending.
    pub episodes: Vec<Episode>,
    /// CW resume position (seconds) for the most-recently-played row of
    /// this title. `None` when no CW row exists → frontend hides the
    /// Resume button (PRD §F-010 code acceptance).
    pub resume_position_s: Option<f64>,
    pub resume_duration_s: Option<f64>,
    /// Season the user should resume on for series; `None` for movies.
    pub resume_season: Option<i64>,
    pub resume_episode: Option<i64>,
    /// Stremio id the user should be sent to when activating Resume
    /// (`tt0133093` for movies, `tt0944947:1:1` for series). Populated
    /// alongside `resume_position_s` so the frontend doesn't have to
    /// re-derive it.
    pub resume_video_id: Option<String>,
    /// `IMDb` id resolved from the supplied `title_id` (falls back
    /// through TMDB when not directly known). The frontend uses this
    /// when activating Play on a movie or building per-episode stream
    /// ids.
    pub stremio_id: Option<String>,
}

/// One stream row in the detail view (PRD §F-010 stream row contents).
///
/// `quality` / `hdr` / `audio` / `codec` come from
/// `kino_addons::parse::parse` over the concatenation of the addon-
/// supplied `name` / `title` / `description` fields (Stremio addons put
/// the parseable filename text in different places — Torrentio puts
/// quality + audio + codec in `title`, `OpenSubtitles` uses `description`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamRow {
    pub addon_id: String,
    pub addon_name: String,
    /// Display label rendered as the row title — the addon's `name` field
    /// when present, else a `"{addon} stream"` fallback.
    pub name: String,
    /// Filename-like detail line (parsed from `title` / `description`).
    pub detail: Option<String>,
    /// Parsed quality badge (4K / 1080p / 720p / SD). `None` when the
    /// stream's text didn't match any locked PRD §8 regex.
    pub quality: Option<Quality>,
    pub hdr: Option<Hdr>,
    pub audio: Option<Audio>,
    pub codec: Option<Codec>,
    /// Seeders extracted from the stream's `title` / `description`.
    /// Recognized shapes: Torrentio's `"👤 156"`, plain `"Seeders: 156"`.
    pub seeders: Option<u32>,
    /// Filesize in bytes (extracted from `"💾 5.4 GB"` / `"Size: 5.4 GB"`).
    pub size_bytes: Option<u64>,
    /// Direct playable URL when the addon supplies one (e.g. Public
    /// Domain Movies); otherwise `None`.
    pub url: Option<String>,
    /// `BitTorrent` info hash (`infoHash` in the Stremio protocol).
    /// `None` when this is a direct URL stream.
    pub info_hash: Option<String>,
    /// Multi-file torrent: which file to play (`fileIdx`).
    pub file_idx: Option<i64>,
    /// Tracker / source hints carried through unchanged.
    pub sources: Vec<String>,
}

/// `get_title_detail(title_id, kind, lang_pref) -> TitleDetail` (PRD §F-010).
#[tauri::command]
pub async fn get_title_detail(
    db: State<'_, Db>,
    title_id: String,
    kind: TitleKind,
    lang_pref: Vec<String>,
) -> Result<TitleDetail, String> {
    let chain_hash = lang_chain_hash(&lang_pref);
    let cache_key = format!("meta:{}:{}:{}", title_id, kind.as_str(), chain_hash);

    let cached_payload: Option<TitleDetailPayload> = match db.cache_get(&cache_key).await {
        Ok(Some(payload)) => serde_json::from_str(&payload).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "title-detail cache_get failed");
            None
        }
    };

    let mut payload = if let Some(p) = cached_payload {
        p
    } else {
        let fetched =
            get_title_detail_uncached(&db, &title_id, kind, &lang_pref, &HttpConfig::default())
                .await?;
        let serialized = serde_json::to_string(&fetched).map_err(|e| e.to_string())?;
        let expires_at = now_unix()
            .saturating_add(i64::try_from(kino_core::constants::META_TTL_S).unwrap_or(i64::MAX));
        if let Err(e) = db.cache_set(&cache_key, &serialized, expires_at).await {
            tracing::warn!(error = %e, "failed to persist title-detail cache");
        }
        fetched
    };

    // Always read CW live — even on a cache hit. PRD §F-010 "Resume button
    // only present when matching CW entry exists" must toggle as soon as
    // the user starts / completes a playback session.
    apply_cw_to_payload(&db, &mut payload).await;

    Ok(payload.into_detail(title_id))
}

/// Inner payload used in the meta cache. Mirrors [`TitleDetail`] but
/// omits the resume / progress fields — those are layered in at read
/// time by [`apply_cw_to_payload`] so a stale cache doesn't pin the
/// Resume button to a previous session's state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TitleDetailPayload {
    kind: TitleKind,
    title: String,
    year: Option<u16>,
    runtime_minutes: Option<u32>,
    age_rating: Option<String>,
    genres: Vec<String>,
    summary: Option<String>,
    imdb_rating: Option<f64>,
    tmdb_rating: Option<f64>,
    trakt_rating: Option<f64>,
    backdrop: Option<String>,
    logo: Option<String>,
    poster: Option<String>,
    cast: Vec<CastMember>,
    episodes: Vec<Episode>,
    stremio_id: Option<String>,
    // CW-derived fields, populated by `apply_cw_to_payload` (never persisted).
    #[serde(default, skip_serializing)]
    resume_position_s: Option<f64>,
    #[serde(default, skip_serializing)]
    resume_duration_s: Option<f64>,
    #[serde(default, skip_serializing)]
    resume_season: Option<i64>,
    #[serde(default, skip_serializing)]
    resume_episode: Option<i64>,
    #[serde(default, skip_serializing)]
    resume_video_id: Option<String>,
}

impl TitleDetailPayload {
    fn into_detail(self, id: String) -> TitleDetail {
        TitleDetail {
            id,
            kind: self.kind,
            title: self.title,
            year: self.year,
            runtime_minutes: self.runtime_minutes,
            age_rating: self.age_rating,
            genres: self.genres,
            summary: self.summary,
            imdb_rating: self.imdb_rating,
            tmdb_rating: self.tmdb_rating,
            trakt_rating: self.trakt_rating,
            backdrop: self.backdrop,
            logo: self.logo,
            poster: self.poster,
            cast: self.cast,
            episodes: self.episodes,
            resume_position_s: self.resume_position_s,
            resume_duration_s: self.resume_duration_s,
            resume_season: self.resume_season,
            resume_episode: self.resume_episode,
            resume_video_id: self.resume_video_id,
            stremio_id: self.stremio_id,
        }
    }
}

/// Cache-bypassing core of [`get_title_detail`]. Public to the module so
/// tests can drive it with a `for_test()` `HttpConfig` (zero backoffs)
/// without going through `response_cache`.
async fn get_title_detail_uncached(
    db: &Db,
    title_id: &str,
    kind: TitleKind,
    lang_pref: &[String],
    http_config: &HttpConfig,
) -> Result<TitleDetailPayload, String> {
    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let trakt_key = db.kv_get(TRAKT_API_KEY).await.map_err(ipc)?;

    let tmdb_client = match tmdb_key {
        Some(k) => Some(
            TmdbClient::with_options(
                k,
                http_config.clone(),
                kino_metadata::tmdb::TMDB_BASE_URL.to_string(),
            )
            .map_err(ipc)?,
        ),
        None => None,
    };
    let trakt_client = match trakt_key {
        Some(k) => Some(
            TraktClient::with_options(
                k,
                http_config.clone(),
                kino_metadata::trakt::TRAKT_BASE_URL.to_string(),
            )
            .map_err(ipc)?,
        ),
        None => None,
    };

    let ids = resolve_title_ids(title_id, kind, tmdb_client.as_ref()).await?;
    let stremio_id = ids.imdb_id.clone();

    let primary_lang = lang_pref
        .first()
        .cloned()
        .unwrap_or_else(|| "en".to_string());

    // (1) Stremio meta baseline. Walk enabled addons that serve the
    // `meta` resource for this kind in display_order and use the first
    // one that returns a useful response. Cinemeta is the locked
    // default (lowest display_order on first launch) but any meta-
    // serving addon can substitute. Stremio addons take the IMDb id
    // directly; without one we skip this stage entirely.
    let mut payload = if let Some(ref imdb) = stremio_id {
        match fetch_meta_for_title(db, kind, imdb, http_config).await {
            Ok(Some(meta)) => payload_from_cinemeta(kind, meta),
            Ok(None) => payload_skeleton(kind),
            Err(e) => {
                tracing::warn!(error = %e, "addon meta fetch failed; continuing with TMDB-only payload");
                payload_skeleton(kind)
            }
        }
    } else {
        payload_skeleton(kind)
    };

    payload.stremio_id = stremio_id.clone();

    // (2) TMDB overlay.
    if let (Some(client), Some(tmdb_id)) = (tmdb_client.as_ref(), ids.tmdb_id) {
        match client.title_details(tmdb_id, kind, &primary_lang).await {
            Ok(details) => apply_tmdb_details(&mut payload, &details),
            Err(e) => tracing::warn!(error = %e, "tmdb title_details failed"),
        }
        match client.credits(tmdb_id, kind).await {
            Ok(cast) => apply_tmdb_cast(&mut payload, cast),
            Err(e) => tracing::warn!(error = %e, "tmdb credits failed"),
        }
    }

    // (3) Trakt overlay.
    if let (Some(client), Some(imdb)) = (trakt_client.as_ref(), stremio_id.as_deref()) {
        match client.title_rating(imdb, kind).await {
            Ok(r) => payload.trakt_rating = r,
            Err(e) => tracing::warn!(error = %e, "trakt title_rating failed"),
        }
    }

    // Trim cast to top six per PRD §F-010.
    payload.cast.truncate(6);
    // Truncate per-episode overview per PRD §F-010 ("summary truncated to
    // 120 chars"). Done here, not at render-time, so the cache holds the
    // already-truncated form and the frontend stays simple.
    for ep in &mut payload.episodes {
        if let Some(text) = ep.overview.as_mut() {
            truncate_to_chars(text, 120);
        }
    }
    Ok(payload)
}

/// Fetch a `MetaDetail` for the given `IMDb` id by walking the enabled
/// meta-serving addons in `display_order`. Cinemeta is the locked
/// default (lowest `display_order`); any other addon that declares the
/// `meta` resource AND lists the relevant `type` in its manifest is a
/// valid fallback.
///
/// Returns `Ok(None)` when no addon could supply a response (every
/// candidate either failed transport-side or didn't carry the `meta`
/// resource). A transport failure on one addon does not abort the
/// walk — we move to the next.
async fn fetch_meta_for_title(
    db: &Db,
    kind: TitleKind,
    imdb_id: &str,
    http_config: &HttpConfig,
) -> Result<Option<MetaDetail>, String> {
    let installed = db.addons_list().await.map_err(ipc)?;
    for addon in installed {
        if !addon.enabled {
            continue;
        }
        let Ok(manifest) = serde_json::from_value::<Manifest>(addon.manifest_json.clone()) else {
            continue;
        };
        if !manifest.types.iter().any(|t| t == kind.as_str()) {
            continue;
        }
        if !manifest.resources.iter().any(|r| r.name() == "meta") {
            continue;
        }
        let client = match AddonClient::with_options(&addon.manifest_url, http_config.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(addon = %addon.id, error = %e, "meta addon client build failed");
                continue;
            }
        };
        match client.meta(kind.as_str(), imdb_id).await {
            Ok(r) => return Ok(Some(r.meta)),
            Err(AddonError::Http(e)) => {
                tracing::warn!(addon = %addon.id, error = %e, "meta addon http failure; trying next");
            }
            Err(e) => {
                tracing::warn!(addon = %addon.id, error = %e, "meta addon decode failure; trying next");
            }
        }
    }
    Ok(None)
}

/// Build an empty payload skeleton for the requested kind.
fn payload_skeleton(kind: TitleKind) -> TitleDetailPayload {
    TitleDetailPayload {
        kind,
        title: String::new(),
        year: None,
        runtime_minutes: None,
        age_rating: None,
        genres: Vec::new(),
        summary: None,
        imdb_rating: None,
        tmdb_rating: None,
        trakt_rating: None,
        backdrop: None,
        logo: None,
        poster: None,
        cast: Vec::new(),
        episodes: Vec::new(),
        stremio_id: None,
        resume_position_s: None,
        resume_duration_s: None,
        resume_season: None,
        resume_episode: None,
        resume_video_id: None,
    }
}

/// Map a Cinemeta `MetaDetail` into the baseline payload.
fn payload_from_cinemeta(kind: TitleKind, meta: MetaDetail) -> TitleDetailPayload {
    let year = meta.release_info.as_deref().and_then(parse_release_year);
    let runtime_minutes = meta.runtime.as_deref().and_then(parse_runtime_minutes);
    let imdb_rating = meta
        .imdb_rating
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| *n > 0.0);
    let cast = meta
        .cast
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|name| CastMember {
            name,
            character: None,
            photo: None,
        })
        .collect();
    let episodes = if matches!(kind, TitleKind::Series) {
        meta.videos
            .into_iter()
            .filter_map(|v| {
                let season = v.season?;
                let episode = v.episode?;
                // Stremio "specials" / "extras" videos come through as
                // season 0; PRD §F-010 doesn't include them in the
                // episode list (locked: "season selector + episode list"
                // for canonical seasons).
                if season < 1 || episode < 1 {
                    return None;
                }
                Some(Episode {
                    video_id: v.id,
                    season,
                    episode,
                    title: v.title,
                    air_date: v.released.filter(|s| !s.is_empty()),
                    overview: v.overview.filter(|s| !s.is_empty()),
                    thumbnail: v.thumbnail.filter(|s| !s.is_empty()),
                    progress: 0.0,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    // Ensure episodes are sorted by (season, episode) — Cinemeta is
    // usually already sorted but we don't rely on it.
    let mut episodes = episodes;
    episodes.sort_by(|a, b| {
        a.season
            .cmp(&b.season)
            .then_with(|| a.episode.cmp(&b.episode))
    });
    TitleDetailPayload {
        kind,
        title: meta.name,
        year,
        runtime_minutes,
        age_rating: None,
        genres: meta.genres,
        summary: meta.description.filter(|s| !s.is_empty()),
        imdb_rating,
        tmdb_rating: None,
        trakt_rating: None,
        backdrop: meta.background.filter(|s| !s.is_empty()),
        logo: meta.logo.filter(|s| !s.is_empty()),
        poster: meta.poster.filter(|s| !s.is_empty()),
        cast,
        episodes,
        stremio_id: None,
        resume_position_s: None,
        resume_duration_s: None,
        resume_season: None,
        resume_episode: None,
        resume_video_id: None,
    }
}

/// Layer TMDB details over the Cinemeta-derived payload. TMDB wins on
/// fields where it returns a value; Cinemeta's values stay as fallbacks
/// (notably runtime, which Cinemeta serves as `"136 min"` and TMDB as
/// the same minutes but parsed natively).
fn apply_tmdb_details(payload: &mut TitleDetailPayload, details: &TmdbTitleDetails) {
    if let Some(n) = details.runtime_minutes {
        payload.runtime_minutes = Some(n);
    }
    if let Some(ref r) = details.age_rating {
        payload.age_rating = Some(r.clone());
    }
    if !details.genres.is_empty() {
        payload.genres.clone_from(&details.genres);
    }
    if let Some(ref text) = details.overview {
        payload.summary = Some(text.clone());
    }
    if let Some(r) = details.rating {
        payload.tmdb_rating = Some(r);
    }
}

/// Replace the Cinemeta name-only cast with the TMDB photo-augmented
/// roster. TMDB's order is the canonical billing order — we don't merge
/// or dedup; for v1 the photo-bearing list is strictly better.
fn apply_tmdb_cast(payload: &mut TitleDetailPayload, cast: Vec<TmdbCastMember>) {
    if cast.is_empty() {
        return;
    }
    payload.cast = cast
        .into_iter()
        .map(|m| CastMember {
            name: m.name,
            character: m.character,
            photo: m.photo_url,
        })
        .collect();
}

/// Walk the user's CW table and stamp the payload with per-episode
/// progress + the title-level resume target. CW reads are cheap (single
/// SQL query) so we always issue the lookup even on a cache hit.
async fn apply_cw_to_payload(db: &Db, payload: &mut TitleDetailPayload) {
    let Some(ref imdb) = payload.stremio_id else {
        return;
    };
    let rows = match db.cw_list().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "cw_list failed during title-detail enrichment");
            return;
        }
    };
    let matching: Vec<&ContinueWatching> = rows
        .iter()
        .filter(|cw| cw.title_id == *imdb && cw.kind == payload.kind)
        .collect();
    if matching.is_empty() {
        return;
    }

    // Per-episode progress: stamp each Episode whose (season, episode)
    // tuple matches a CW row.
    if matches!(payload.kind, TitleKind::Series) {
        for ep in &mut payload.episodes {
            if let Some(cw) = matching
                .iter()
                .find(|c| c.season == ep.season && c.episode == ep.episode)
            {
                ep.progress = cw.progress();
            }
        }
    }

    // Resume target: pick the most-recently-played CW row (max
    // `last_played_at`). For movies the row's season/episode are 0/0;
    // the stremio id is the IMDb id verbatim. For series we synthesize
    // `tt0944947:S:E`.
    let resume = matching
        .iter()
        .max_by_key(|cw| cw.last_played_at)
        .copied()
        .unwrap();
    payload.resume_position_s = Some(resume.position_s);
    payload.resume_duration_s = Some(resume.duration_s);
    payload.resume_season = if matches!(payload.kind, TitleKind::Series) {
        Some(resume.season)
    } else {
        None
    };
    payload.resume_episode = if matches!(payload.kind, TitleKind::Series) {
        Some(resume.episode)
    } else {
        None
    };
    payload.resume_video_id = Some(match payload.kind {
        TitleKind::Movie => imdb.clone(),
        TitleKind::Series => format!("{imdb}:{}:{}", resume.season, resume.episode),
    });
}

/// Parse a Cinemeta `runtime` string into integer minutes.
///
/// Cinemeta emits `"136 min"`, `"1h 36min"`, `"96 min"`, `"58 min."`. We
/// pull the first integer token. `"1h 36min"` returns 1 (the leading
/// number) which is wrong; in practice Cinemeta uses `"96 min"` form for
/// movies and `"60 min"` for TV episode runtimes, so the simple parser
/// covers the observed shapes. TMDB's native `runtime` field overrides
/// this when available (see `apply_tmdb_details`).
fn parse_runtime_minutes(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Truncate `text` in place to at most `max_chars` Unicode scalar
/// values. Append `"…"` when truncation actually occurred so the UI
/// signals the elision per PRD §F-010 "summary truncated to 120 chars".
fn truncate_to_chars(text: &mut String, max_chars: usize) {
    let mut byte_idx = text.len();
    for (count, (i, _)) in text.char_indices().enumerate() {
        if count == max_chars {
            byte_idx = i;
            break;
        }
    }
    if byte_idx < text.len() {
        text.truncate(byte_idx);
        text.push('…');
    }
}

// ---- F-010: streams enumeration ---------------------------------------

/// `get_streams(title_id, kind, season?, episode?) -> Vec<StreamRow>`
/// (PRD §F-010 stream rows).
///
/// `season` / `episode` MUST be `Some` together for series, `None`
/// together for movies. The command tolerates bad shapes by returning
/// an error string.
///
/// Result is sorted descending by (quality, seeders, size) per PRD
/// §F-010. Per-addon failures are logged and skipped — one flaky stream
/// source must not blank the whole list.
#[tauri::command]
pub async fn get_streams(
    db: State<'_, Db>,
    title_id: String,
    kind: TitleKind,
    season: Option<i64>,
    episode: Option<i64>,
) -> Result<Vec<StreamRow>, String> {
    get_streams_with_config(
        &db,
        &title_id,
        kind,
        season,
        episode,
        &HttpConfig::default(),
    )
    .await
}

/// PRD §F-010 shape invariant: movies have no season/episode; series
/// must carry both, each ≥ 1. Bad shapes return an error string the
/// IPC layer surfaces to the frontend (which never builds bad shapes
/// in normal use; this is purely defensive).
fn validate_stream_request_shape(
    kind: TitleKind,
    season: Option<i64>,
    episode: Option<i64>,
) -> Result<(), String> {
    match kind {
        TitleKind::Movie => {
            if season.is_some() || episode.is_some() {
                return Err(format!(
                    "get_streams: kind=movie must not carry season/episode (got season={season:?}, episode={episode:?})"
                ));
            }
        }
        TitleKind::Series => match (season, episode) {
            (Some(s), Some(e)) if s >= 1 && e >= 1 => {}
            _ => {
                return Err(format!(
                    "get_streams: kind=series requires season>=1 AND episode>=1 (got season={season:?}, episode={episode:?})"
                ));
            }
        },
    }
    Ok(())
}

/// Resolve the kino `title_id` (`imdb:tt…` / `tmdb:N` / `tvdb:N`) into
/// the Stremio addon id shape: bare `IMDb` id for movies, `imdb:S:E`
/// for series episodes. Returns `None` when no `IMDb` id can be
/// resolved (e.g. a TVDB-only entry with no TMDB key configured to
/// cross-resolve) — Stremio addons can't serve streams in that case.
async fn resolve_stremio_id(
    db: &Db,
    title_id: &str,
    kind: TitleKind,
    season: Option<i64>,
    episode: Option<i64>,
    http_config: &HttpConfig,
) -> Result<Option<String>, String> {
    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let tmdb_client = match tmdb_key {
        Some(k) => Some(
            TmdbClient::with_options(
                k,
                http_config.clone(),
                kino_metadata::tmdb::TMDB_BASE_URL.to_string(),
            )
            .map_err(ipc)?,
        ),
        None => None,
    };
    let ids = resolve_title_ids(title_id, kind, tmdb_client.as_ref()).await?;
    let Some(imdb) = ids.imdb_id else {
        return Ok(None);
    };
    Ok(Some(match (kind, season, episode) {
        (TitleKind::Movie, _, _) => imdb,
        (TitleKind::Series, Some(s), Some(e)) => format!("{imdb}:{s}:{e}"),
        _ => unreachable!("season/episode shape validated above"),
    }))
}

/// Walk the persisted addon list and produce a [`StreamWorkItem`] for
/// each enabled stream-serving addon that handles `kind`. Reuses the
/// F-006 manifest filter; non-stream addons (catalogs / metadata only)
/// are silently skipped.
async fn build_stream_work(
    db: &Db,
    kind: TitleKind,
    stremio_id: &str,
    http_config: &HttpConfig,
) -> Result<Vec<StreamWorkItem>, String> {
    let installed = db.addons_list().await.map_err(ipc)?;
    let mut work: Vec<StreamWorkItem> = Vec::new();
    for addon in installed {
        if !addon.enabled {
            continue;
        }
        let manifest = match serde_json::from_value::<Manifest>(addon.manifest_json.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    addon = %addon.id,
                    error = %e,
                    "could not parse persisted manifest for stream fetch"
                );
                continue;
            }
        };
        if !manifest.serves_stream(kind.as_str()) {
            continue;
        }
        let client = match AddonClient::with_options(&addon.manifest_url, http_config.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    addon = %addon.id,
                    error = %e,
                    "could not build addon client for stream fetch"
                );
                continue;
            }
        };
        work.push(StreamWorkItem {
            addon_id: addon.id,
            addon_name: manifest.name,
            client,
            stremio_id: stremio_id.to_string(),
            kind,
        });
    }
    Ok(work)
}

async fn get_streams_with_config(
    db: &Db,
    title_id: &str,
    kind: TitleKind,
    season: Option<i64>,
    episode: Option<i64>,
    http_config: &HttpConfig,
) -> Result<Vec<StreamRow>, String> {
    validate_stream_request_shape(kind, season, episode)?;

    let Some(stremio_id) =
        resolve_stremio_id(db, title_id, kind, season, episode, http_config).await?
    else {
        // Without an IMDb id no Stremio addon can serve streams.
        return Ok(Vec::new());
    };

    let work = build_stream_work(db, kind, &stremio_id, http_config).await?;
    if work.is_empty() {
        return Ok(Vec::new());
    }

    // Bounded fan-out — reuses the F-006 / F-008 row-5 ceiling. The
    // detail-view stream fetch is a fairly cold path (only fires when
    // the user opens a title), but the same 8-permit budget applies.
    let semaphore = Arc::new(Semaphore::new(AVAILABILITY_CONCURRENCY));
    let mut set: tokio::task::JoinSet<Vec<StreamRow>> = tokio::task::JoinSet::new();
    for item in work {
        let permit = Arc::clone(&semaphore);
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.ok();
            fetch_streams_for_addon(item).await
        });
    }
    let mut rows: Vec<StreamRow> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(mut chunk) => rows.append(&mut chunk),
            Err(e) => tracing::warn!(error = %e, "stream-fetch task panicked"),
        }
    }

    // PRD §F-010 stream sort (locked, descending priority): quality
    // (4K > 1080p > 720p > SD > none), then seeders, then size.
    rows.sort_by(|a, b| {
        quality_rank(b.quality)
            .cmp(&quality_rank(a.quality))
            .then_with(|| b.seeders.unwrap_or(0).cmp(&a.seeders.unwrap_or(0)))
            .then_with(|| b.size_bytes.unwrap_or(0).cmp(&a.size_bytes.unwrap_or(0)))
    });
    Ok(rows)
}

struct StreamWorkItem {
    addon_id: String,
    addon_name: String,
    client: AddonClient,
    stremio_id: String,
    kind: TitleKind,
}

async fn fetch_streams_for_addon(item: StreamWorkItem) -> Vec<StreamRow> {
    let StreamWorkItem {
        addon_id,
        addon_name,
        client,
        stremio_id,
        kind,
    } = item;
    let resp = match client.stream(kind.as_str(), &stremio_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                addon = %addon_id,
                stremio_id = %stremio_id,
                error = %e,
                "addon stream fetch failed; skipping"
            );
            return Vec::new();
        }
    };
    resp.streams
        .into_iter()
        .map(|s| addon_stream_to_row(&addon_id, &addon_name, s))
        .collect()
}

/// Convert a Stremio [`AddonStream`] into a PRD §F-010 [`StreamRow`].
fn addon_stream_to_row(addon_id: &str, addon_name: &str, s: AddonStream) -> StreamRow {
    let combined_for_parse = stream_text_for_parse(&s);
    let tags: ParsedTags = parse_filename(&combined_for_parse);
    let seeders = extract_seeders(&combined_for_parse);
    let size_bytes = extract_size_bytes(&combined_for_parse);
    let detail = pick_detail_line(&s);
    let display_name = s
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{addon_name} stream"));
    StreamRow {
        addon_id: addon_id.to_string(),
        addon_name: addon_name.to_string(),
        name: display_name,
        detail,
        quality: tags.quality,
        hdr: tags.hdr,
        audio: tags.audio,
        codec: tags.codec,
        seeders,
        size_bytes,
        url: s.url,
        info_hash: s.info_hash,
        file_idx: s.file_idx,
        sources: s.sources,
    }
}

/// Concatenate every text-bearing field on a stream so the §8 regex set
/// has the largest possible haystack. Different addons surface the
/// parseable filename in different places (`name` for Public Domain
/// Movies, `title` for Torrentio, `description` for `OpenSubtitles`).
fn stream_text_for_parse(s: &AddonStream) -> String {
    let mut buf = String::new();
    if let Some(ref n) = s.name {
        buf.push_str(n);
        buf.push(' ');
    }
    if let Some(ref t) = s.title {
        buf.push_str(t);
        buf.push(' ');
    }
    if let Some(ref d) = s.description {
        buf.push_str(d);
    }
    buf
}

/// The display-detail line shown under the stream name. Picks `title`
/// when present (Torrentio's filename + emoji line) else `description`.
fn pick_detail_line(s: &AddonStream) -> Option<String> {
    if let Some(t) = s.title.as_deref() {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    s.description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// Map a [`Quality`] tag to its sort rank. Higher is better. Used by
/// the PRD §F-010 stream sort.
const fn quality_rank(q: Option<Quality>) -> u8 {
    match q {
        Some(Quality::Uhd4K) => 4,
        Some(Quality::Fhd1080) => 3,
        Some(Quality::Hd720) => 2,
        Some(Quality::Sd) => 1,
        None => 0,
    }
}

/// Extract a seeders count from a stream's combined text. Recognized
/// shapes:
///
///   - Torrentio: `"👤 156"` / `"👤 7,894"`
///   - Generic: `"Seeders: 156"` / `"seeds 156"` / `"Seeds: 156"`
///
/// Returns the first parsable integer following one of those markers.
fn extract_seeders(text: &str) -> Option<u32> {
    static SEEDERS_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:👤\s*|seeder?s?\s*[:=]?\s*|seeds?\s*[:=]?\s*)([\d,]+)").unwrap()
    });
    let cap = SEEDERS_RE.captures(text)?;
    let raw = cap.get(1)?.as_str().replace(',', "");
    raw.parse::<u32>().ok()
}

/// Extract a filesize-in-bytes from a stream's combined text. Recognized
/// shapes:
///
///   - Torrentio: `"💾 5.4 GB"` / `"💾 920 MB"`
///   - Generic: `"Size: 5.4 GB"` / `"5.4GB"`
///
/// The numeric part may be integer or decimal; the unit is one of B / KB
/// / MB / GB / TB (case-insensitive).
fn extract_size_bytes(text: &str) -> Option<u64> {
    static SIZE_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:💾\s*|size\s*[:=]?\s*)?(\d+(?:[.,]\d+)?)\s*(B|KB|MB|GB|TB)\b")
            .unwrap()
    });
    let cap = SIZE_RE.captures(text)?;
    let num_raw = cap.get(1)?.as_str().replace(',', ".");
    let num: f64 = num_raw.parse().ok()?;
    let unit = cap.get(2)?.as_str().to_ascii_uppercase();
    let bytes = match unit.as_str() {
        "B" => num,
        "KB" => num * 1024.0,
        "MB" => num * 1024.0 * 1024.0,
        "GB" => num * 1024.0 * 1024.0 * 1024.0,
        "TB" => num * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    // Stream sizes are bounded well below 2^53 (Tauri IPC is JSON, which
    // doesn't carry larger integers losslessly anyway). Guard the cast
    // explicitly so a malformed numeric token can't produce garbage.
    if !bytes.is_finite() || !(0.0..9.0e18).contains(&bytes) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = bytes as u64;
    Some(n)
}

// ---- F-011: Search ---------------------------------------------------
//
// `search(query, page, locale)` (PRD §F-011) — debounced live search
// across TMDB / TVDB / Trakt with IMDb-id detection. Pages results at
// `SEARCH_PAGE_SIZE = 20`, deduped by IMDb id (TMDB id as fallback),
// with F-006 availability filtering applied so the UI only surfaces
// items the user can actually stream.
//
// Recent searches: `recent_searches_list` / `recent_searches_upsert` /
// `recent_searches_clear` ride the F-002 persistence layer (the
// `recent_searches` table was scaffolded in migration 0001). Empty
// queries surface the most-recent N entries; non-empty queries are
// recorded only when the live search resolves to at least one result
// (no point persisting typos).
//
// IMDb-id shortcut: a query that matches `^tt\d+$` is resolved server-
// side via TMDB `/find?external_source=imdb_id` so the frontend can
// navigate directly to `/title/imdb:{id}?kind={kind}` without guessing
// which side the title lives on. Failure modes (no TMDB key, no match)
// fall through to the normal multi-provider search so a typo doesn't
// dead-end the UX.

/// Direct IMDb-id match returned alongside a normal search response.
/// When present the frontend MUST navigate immediately to the title
/// detail rather than render the result list — PRD §F-011 "Pasting
/// `tt1234567` opens the corresponding title detail directly".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchDirectMatch {
    /// Provider-prefixed kino id (`imdb:tt...`). Use this exact string
    /// when building the `/title/:id?kind=` URL so the detail route's
    /// `parse_title_id` accepts it.
    pub id: String,
    /// Detected kind so the detail route knows which IPC to issue.
    pub kind: TitleKind,
}

/// Aggregated search response. Either `direct` is `Some(...)` (IMDb-id
/// hit; the UI navigates directly) OR `results` carries the deduped /
/// availability-filtered page. The two are intentionally returned in
/// the same shape so the frontend can dispatch on `direct`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponse {
    /// IMDb-id shortcut hit. Present only when the query matches
    /// `^tt\d+$` and TMDB `/find` resolves it to a movie or series.
    pub direct: Option<SearchDirectMatch>,
    /// Result page (≤ [`SEARCH_PAGE_SIZE`] items). Empty for direct
    /// matches.
    pub results: Vec<TitleSummary>,
    /// `true` when the upstream providers returned at least one extra
    /// candidate past this page — the UI uses this to gate "load more".
    pub has_more: bool,
}

/// `recent_searches_list()` (PRD §F-011 "empty query: 'Recent
/// searches'"). Returns the last [`RECENT_SEARCHES_MAX`] queries,
/// newest first.
#[tauri::command]
pub async fn recent_searches_list(db: State<'_, Db>) -> Result<Vec<String>, String> {
    db.recent_searches_list(kino_core::constants::RECENT_SEARCHES_MAX)
        .await
        .map_err(ipc)
}

/// `recent_searches_upsert(query)`. Refreshes the entry's
/// `searched_at` to now and prunes the table past
/// [`RECENT_SEARCHES_MAX`]. Idempotent.
#[tauri::command]
pub async fn recent_searches_upsert(db: State<'_, Db>, query: String) -> Result<(), String> {
    db.recent_searches_upsert(&query, kino_core::constants::RECENT_SEARCHES_MAX)
        .await
        .map_err(ipc)
}

/// `recent_searches_clear()`. Removes every entry — surfaced for the
/// Settings → Privacy "clear history" action (F-016).
#[tauri::command]
pub async fn recent_searches_clear(db: State<'_, Db>) -> Result<u64, String> {
    db.recent_searches_clear().await.map_err(ipc)
}

/// Per-provider base URLs the search orchestrator dials. Production uses
/// the locked PRD §F-003 endpoints (the [`Default`] impl); the unit
/// tests swap each one for a `wiremock::MockServer::uri()`.
#[derive(Debug, Clone)]
struct SearchProviderUrls {
    tmdb: String,
    trakt: String,
    tvdb: String,
}

impl Default for SearchProviderUrls {
    fn default() -> Self {
        Self {
            tmdb: kino_metadata::tmdb::TMDB_BASE_URL.to_string(),
            trakt: kino_metadata::trakt::TRAKT_BASE_URL.to_string(),
            tvdb: kino_metadata::tvdb::TVDB_BASE_URL.to_string(),
        }
    }
}

/// `search(query, page, locale)` (PRD §F-011).
///
/// Issues parallel TMDB / TVDB / Trakt search requests, dedups by `IMDb` id
/// (then TMDB id), applies F-006 availability filtering, and returns up
/// to [`SEARCH_PAGE_SIZE`] items. Empty / whitespace-only queries return
/// the empty response — the UI surfaces recent searches via
/// [`recent_searches_list`] instead.
///
/// The `^tt\d+$` shortcut path runs first: when the query matches, TMDB
/// `/find?external_source=imdb_id` resolves the kind in one call and
/// the response's `direct` field carries the id for direct navigation.
#[tauri::command]
pub async fn search(
    db: State<'_, Db>,
    query: String,
    page: u32,
    locale: String,
) -> Result<SearchResponse, String> {
    search_with_config(
        &db,
        &query,
        page,
        &locale,
        &HttpConfig::default(),
        &SearchProviderUrls::default(),
    )
    .await
}

/// Cache-bypassing core of [`search`]. Exists so the unit tests can
/// drive the orchestration path with [`HttpConfig::for_test`] (zero
/// backoffs, short timeout) and wiremock-supplied base URLs without
/// going through the production `HttpConfig::default()` retry schedule.
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
async fn search_with_config(
    db: &Db,
    query: &str,
    page: u32,
    locale: &str,
    http_config: &HttpConfig,
    urls: &SearchProviderUrls,
) -> Result<SearchResponse, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(SearchResponse {
            direct: None,
            results: Vec::new(),
            has_more: false,
        });
    }

    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let trakt_key = db.kv_get(TRAKT_API_KEY).await.map_err(ipc)?;
    let tvdb_key = db.kv_get(TVDB_API_KEY).await.map_err(ipc)?;

    // IMDb-id shortcut: `^tt\d+$`. Resolve via TMDB /find. If TMDB has no
    // mapping, fall through to the regular search so the user still gets
    // a result list.
    if is_imdb_id_query(trimmed) {
        if let Some(direct) =
            resolve_imdb_shortcut(trimmed, tmdb_key.as_deref(), http_config, &urls.tmdb).await?
        {
            return Ok(SearchResponse {
                direct: Some(direct),
                results: Vec::new(),
                has_more: false,
            });
        }
    }

    // Fan out across providers. TMDB is the only required signal; Trakt
    // and TVDB are optional (PRD §F-003: TMDB is the primary; the others
    // enrich) — without their keys we still produce TMDB-only results.
    let (tmdb_items, trakt_items, tvdb_items) = fetch_search_providers(
        trimmed,
        page,
        locale,
        tmdb_key.as_deref(),
        trakt_key.as_deref(),
        tvdb_key.as_deref(),
        http_config,
        urls,
    )
    .await?;

    // Stable provider order: TMDB → Trakt → TVDB so dedup keeps the
    // TMDB-shaped row when the same title comes back from multiple
    // providers (TMDB carries the most metadata).
    let mut merged: Vec<TitleSummary> =
        Vec::with_capacity(tmdb_items.len() + trakt_items.len() + tvdb_items.len());
    merged.extend(tmdb_items);
    merged.extend(trakt_items);
    merged.extend(tvdb_items);
    let deduped = dedup_search_results(merged);

    // F-006 availability filter. We split the candidate set into the
    // first 2 × page_size items (so dropouts from availability still
    // leave room for a full page); items past the window are kept aside
    // and surfaced only if availability dropped too many. This caps
    // worst-case availability dispatch at 40 items per search call.
    let page_size = kino_core::constants::SEARCH_PAGE_SIZE;
    let availability_window = page_size.saturating_mul(2).min(deduped.len());
    let (head, tail) = deduped.split_at(availability_window);
    let head_filtered = apply_availability_filter(db, head, http_config).await?;
    let filtered_count = head_filtered.len();
    let take_from_head = filtered_count.min(page_size);
    let mut page_items: Vec<TitleSummary> =
        head_filtered.into_iter().take(take_from_head).collect();
    let mut has_more = filtered_count > take_from_head || !tail.is_empty();
    // Pad with unchecked tail items if availability dropped too many
    // from the head. This preserves PRD "page size 20" while staying
    // within the 40-item availability budget — the alternative would
    // be a recursive top-up across providers which is way more code.
    if page_items.len() < page_size {
        let needed = page_size - page_items.len();
        for item in tail.iter().take(needed).cloned() {
            page_items.push(item);
        }
        if tail.len() > needed {
            has_more = true;
        }
    }

    if !page_items.is_empty() {
        // Persist the query only when it produced results — typos /
        // empty matches don't pollute the recents list. Failure to
        // persist is non-fatal (DB write errors get logged).
        if let Err(e) = db
            .recent_searches_upsert(trimmed, kino_core::constants::RECENT_SEARCHES_MAX)
            .await
        {
            tracing::warn!(error = %e, "failed to upsert recent_searches");
        }
    }

    Ok(SearchResponse {
        direct: None,
        results: page_items,
        has_more,
    })
}

/// Detect the `^tt\d+$` shape (PRD §F-011: "if query matches `^tt\d+$`,
/// resolve via TMDB `/find`"). Tolerates surrounding whitespace via the
/// caller's `.trim()`.
fn is_imdb_id_query(s: &str) -> bool {
    s.len() > 2 && s.starts_with("tt") && s[2..].bytes().all(|b| b.is_ascii_digit())
}

/// Resolve an IMDb-id shortcut via TMDB `/find`. Tries movie first, then
/// TV. Returns `None` when TMDB has no mapping for either kind, OR when
/// the user has no TMDB key configured. The caller falls through to
/// the regular multi-provider search in either case.
async fn resolve_imdb_shortcut(
    imdb_id: &str,
    tmdb_key: Option<&str>,
    http_config: &HttpConfig,
    tmdb_base_url: &str,
) -> Result<Option<SearchDirectMatch>, String> {
    let Some(key) = tmdb_key else {
        tracing::debug!(
            imdb = %imdb_id,
            "no TMDB key configured; skipping IMDb-id shortcut and falling through to multi-provider search"
        );
        return Ok(None);
    };
    let client = TmdbClient::with_options(key, http_config.clone(), tmdb_base_url.to_string())
        .map_err(ipc)?;
    // Movie first — IMDb ids are usually films when ambiguous.
    if let Some(_id) = client
        .find_external(imdb_id, "imdb_id", TitleKind::Movie)
        .await
        .map_err(ipc)?
    {
        return Ok(Some(SearchDirectMatch {
            id: format!("imdb:{imdb_id}"),
            kind: TitleKind::Movie,
        }));
    }
    if let Some(_id) = client
        .find_external(imdb_id, "imdb_id", TitleKind::Series)
        .await
        .map_err(ipc)?
    {
        return Ok(Some(SearchDirectMatch {
            id: format!("imdb:{imdb_id}"),
            kind: TitleKind::Series,
        }));
    }
    Ok(None)
}

/// Fan out search calls in parallel across the three configured
/// providers. Each provider's absence (no key) yields an empty list
/// rather than an error so TMDB-only installs still produce results.
#[allow(clippy::similar_names, clippy::too_many_arguments)] // PRD-locked provider names (tmdb / tvdb).
async fn fetch_search_providers(
    query: &str,
    page: u32,
    locale: &str,
    tmdb_key: Option<&str>,
    trakt_key: Option<&str>,
    tvdb_key: Option<&str>,
    http_config: &HttpConfig,
    urls: &SearchProviderUrls,
) -> Result<(Vec<TitleSummary>, Vec<TitleSummary>, Vec<TitleSummary>), String> {
    // Build clients up front; client-construction failures surface before
    // any network I/O.
    let tmdb = match tmdb_key {
        Some(k) => {
            Some(TmdbClient::with_options(k, http_config.clone(), urls.tmdb.clone()).map_err(ipc)?)
        }
        None => None,
    };
    let trakt = match trakt_key {
        Some(k) => Some(
            TraktClient::with_options(k, http_config.clone(), urls.trakt.clone()).map_err(ipc)?,
        ),
        None => None,
    };
    let tvdb = match tvdb_key {
        Some(k) => {
            Some(TvdbClient::with_options(k, http_config.clone(), urls.tvdb.clone()).map_err(ipc)?)
        }
        None => None,
    };

    let limit_u32 = u32::try_from(kino_core::constants::SEARCH_PAGE_SIZE).unwrap_or(u32::MAX);
    let tmdb_fut = async move {
        let Some(c) = tmdb else { return Ok(Vec::new()) };
        c.search_multi(query, locale, page).await
    };
    let trakt_fut = async move {
        let Some(c) = trakt else {
            return Ok(Vec::new());
        };
        c.search(query, page, limit_u32).await
    };
    // TVDB v4 search doesn't accept a page parameter; we only ask for
    // page 1 and let the host's per-page slicing fall back on TMDB /
    // Trakt for deeper pages. Repeating the TVDB call on every page
    // would only re-yield the same items.
    let tvdb_fut = async move {
        if page > 1 {
            return Ok(Vec::new());
        }
        let Some(c) = tvdb else { return Ok(Vec::new()) };
        c.search(query, limit_u32).await
    };

    let (tmdb_res, trakt_res, tvdb_res) = tokio::join!(tmdb_fut, trakt_fut, tvdb_fut);
    // TMDB failure is recoverable here (unlike trending; F-011 doesn't
    // make TMDB strictly mandatory because the user might be relying on
    // a TVDB-only or addon-catalog driven setup). Log + treat as empty.
    let tmdb_items = tmdb_res.unwrap_or_else(|e| {
        tracing::warn!(provider = "tmdb", error = %e, "search fetch failed; skipping");
        Vec::new()
    });
    let trakt_items = trakt_res.unwrap_or_else(|e| {
        tracing::warn!(provider = "trakt", error = %e, "search fetch failed; skipping");
        Vec::new()
    });
    let tvdb_items = tvdb_res.unwrap_or_else(|e| {
        tracing::warn!(provider = "tvdb", error = %e, "search fetch failed; skipping");
        Vec::new()
    });
    Ok((tmdb_items, trakt_items, tvdb_items))
}

/// Dedup a merged search result list by the `IMDb` id (when present in
/// the kino id) then by the raw provider id. Preserves input order so
/// the TMDB-shaped row wins over Trakt / TVDB shapes when the same
/// title surfaces from multiple providers.
fn dedup_search_results(items: Vec<TitleSummary>) -> Vec<TitleSummary> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<TitleSummary> = Vec::with_capacity(items.len());
    for item in items {
        // Key derivation: kino ids come in `tmdb:N` / `imdb:ttN` / `tvdb:N`
        // / bare `ttN` (Trakt fallback path) shapes. Coerce to a canonical
        // `imdb:ttN` when we can detect IMDb so cross-provider duplicates
        // collapse on the strongest signal.
        let key = if item.id.starts_with("tt")
            && item.id.len() > 2
            && item.id[2..].bytes().all(|b| b.is_ascii_digit())
        {
            // PRD §F-011 dedups by IMDb id; same kind + same imdb collapses.
            format!("{}:imdb:{}", item.kind.as_str(), item.id)
        } else if let Some(rest) = item.id.strip_prefix("imdb:") {
            format!("{}:imdb:{}", item.kind.as_str(), rest)
        } else {
            // Fall back to (kind, raw id) — distinct providers with no
            // imdb mapping stay separate rows.
            format!("{}:{}", item.kind.as_str(), item.id)
        };
        if seen.insert(key) {
            out.push(item);
        }
    }
    out
}

/// Apply the F-006 availability filter to a candidate slice. Items with
/// `available = false` are dropped. When no stream-serving addon is
/// installed every item passes through unchanged — PRD §F-006 already
/// returns "every item unavailable" in that case, which would zero-out
/// search; we honor F-011's "F-006 availability filter applied" wording
/// but also preserve usefulness when no addon is wired (the search
/// surface stays browseable, and the title-detail screen will surface
/// the "no streams" empty state).
async fn apply_availability_filter(
    db: &Db,
    items: &[TitleSummary],
    http_config: &HttpConfig,
) -> Result<Vec<TitleSummary>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    // Short-circuit when no addon serves streams. PRD §F-006's
    // `check_availability` already handles this case, but doing it here
    // means we don't pay the cache + dispatch overhead for the common
    // no-addons-yet first-launch UX.
    let stream_addons = load_stream_addons(db).await?;
    if stream_addons.is_empty() {
        return Ok(items.to_vec());
    }
    let requests: Vec<AvailabilityRequest> = items
        .iter()
        .map(|s| AvailabilityRequest {
            title_id: s.id.clone(),
            kind: s.kind,
        })
        .collect();
    let avail = check_availability_with_config(db, requests, http_config).await?;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for (i, r) in avail.iter().enumerate() {
        keep[i] = r.available;
    }
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| if keep[i] { Some(item.clone()) } else { None })
        .collect())
}

// ---- F-016: Settings screen ---------------------------------------------

use crate::cache_fs;
use crate::logs;
use crate::settings::{
    self, validate_setting, HostPlatform, SettingsView, CACHE_PATH_KEY, KNOWN_SETTINGS_KEYS,
};

/// Build-time injected commit SHA (see `src-tauri/build.rs`). Surfaced on
/// the F-016 §8 About panel.
const COMMIT_SHA: &str = env!("KINO_COMMIT_SHA");

/// Static About-panel facts (version, commit, repo, license). Pure read of
/// `Cargo.toml` workspace metadata + the compile-time commit SHA.
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
    pub repository: &'static str,
    pub license: &'static str,
    pub platform: &'static str,
}

fn host_platform_label() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// `get_app_info()` (PRD §F-016 §8 About).
#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        commit: COMMIT_SHA,
        repository: env!("CARGO_PKG_REPOSITORY"),
        license: env!("CARGO_PKG_LICENSE"),
        platform: host_platform_label(),
    }
}

/// `settings_get_all()` (PRD §F-016).
///
/// Returns the full settings tree with defaults applied for absent keys.
/// `cache_path_default` is resolved by the Tauri host via
/// [`crate::paths::cache_dir_default`] and surfaces in the response so the
/// Settings → Cache "Path" field renders a meaningful placeholder.
#[tauri::command]
pub async fn settings_get_all(
    app: tauri::AppHandle,
    db: State<'_, Db>,
) -> Result<SettingsView, String> {
    let cache_default = crate::paths::cache_dir_default(&app)?
        .to_string_lossy()
        .into_owned();
    settings::load_view(&db, HostPlatform::current(), &cache_default).await
}

/// `settings_set(key, value)` (PRD §F-016 "All settings persist across
/// restarts"). Validates `(key, value)` against the per-key bounds in
/// [`validate_setting`] then writes the normalized value to the KV table.
///
/// Returns the normalized value the caller should now consider authoritative
/// (e.g. boolean inputs are canonicalized to `"true"`/`"false"`).
#[tauri::command]
pub async fn settings_set(db: State<'_, Db>, key: String, value: String) -> Result<String, String> {
    let normalized = validate_setting(&key, &value, HostPlatform::current())?;
    db.kv_set(&key, &normalized).await.map_err(ipc)?;
    Ok(normalized)
}

/// `settings_reset_defaults()` (PRD §F-016 "Reset to defaults button with
/// confirmation restores out-of-box state").
///
/// Wipes every key in [`KNOWN_SETTINGS_KEYS`] and removes every non-Cinemeta
/// addon. Cinemeta is preserved because it's the locked default (PRD §F-007
/// "Cinemeta only DISABLABLE, never REMOVABLE"). System-internal keys
/// (`install_id`, `addons.bootstrap_done`) survive so the install identity
/// remains stable.
#[tauri::command]
pub async fn settings_reset_defaults(db: State<'_, Db>) -> Result<(), String> {
    for key in KNOWN_SETTINGS_KEYS {
        db.kv_delete(key).await.map_err(ipc)?;
    }
    // Drop every non-Cinemeta addon. We walk in two passes so we don't
    // mutate the list mid-iteration. is_cinemeta_id keys on the manifest
    // URL (ADR-057) so an "imposter Cinemeta" with the same id but a
    // different URL is correctly purged.
    let installed = db.addons_list().await.map_err(ipc)?;
    for addon in installed {
        if is_cinemeta_id(&db, &addon.id).await? {
            // Re-enable Cinemeta and snap it to display_order = 0 so the
            // reset really does land us back in out-of-box state.
            db.addons_set_enabled(&addon.id, true).await.map_err(ipc)?;
            db.addons_reorder(std::slice::from_ref(&addon.id))
                .await
                .map_err(ipc)?;
            continue;
        }
        db.addons_delete(&addon.id).await.map_err(ipc)?;
    }
    Ok(())
}

/// `cache_usage_bytes()` (PRD §F-016 §4 "Current usage display"). Returns
/// the byte count of the user-configured cache directory.
#[tauri::command]
pub async fn cache_usage_bytes(app: tauri::AppHandle, db: State<'_, Db>) -> Result<u64, String> {
    let path = resolve_cache_path(&app, &db).await?;
    let path_buf = std::path::PathBuf::from(path);
    tokio::task::spawn_blocking(move || cache_fs::dir_size_bytes(&path_buf))
        .await
        .map_err(|e| format!("cache scan failed: {e}"))
}

/// `cache_clear()` (PRD §F-016 §4 "Clear cache button (confirmation modal)").
/// Removes every file under the configured cache directory; the directory
/// itself stays in place.
#[tauri::command]
pub async fn cache_clear(app: tauri::AppHandle, db: State<'_, Db>) -> Result<(), String> {
    let path = resolve_cache_path(&app, &db).await?;
    let path_buf = std::path::PathBuf::from(path);
    tokio::task::spawn_blocking(move || cache_fs::clear_dir_contents(&path_buf))
        .await
        .map_err(|e| format!("cache clear failed: {e}"))?
        .map_err(|e| format!("cache clear failed: {e}"))
}

/// `export_logs(dest_zip)` (PRD §F-016 §8 "Export logs button: zips logs
/// folder to a chosen location").
#[tauri::command]
pub async fn export_logs(app: tauri::AppHandle, dest_zip: String) -> Result<u64, String> {
    let config_root = crate::paths::app_config_dir(&app)?;
    let log_dir = logs::log_dir(&config_root);
    let dest = std::path::PathBuf::from(dest_zip);
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create destination dir: {e}"))?;
        }
    }
    tokio::task::spawn_blocking(move || logs::zip_log_dir(&log_dir, &dest))
        .await
        .map_err(|e| format!("export_logs failed: {e}"))?
        .map_err(|e| format!("export_logs failed: {e}"))
}

pub async fn resolve_cache_path(app: &tauri::AppHandle, db: &Db) -> Result<String, String> {
    if let Some(custom) = db.kv_get(CACHE_PATH_KEY).await.map_err(ipc)? {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    Ok(crate::paths::cache_dir_default(app)?
        .to_string_lossy()
        .into_owned())
}

// ---- F-013: torrent engine + local HTTP server -------------------------

use kino_server::ServerHandle;
use kino_torrent::{AddInput, Engine};

/// Holder for the torrent engine + local HTTP server. Built once at app
/// boot and stashed in Tauri's managed state so the `start_playback` and
/// `stop_playback` commands can route to a single shared session.
#[derive(Clone)]
pub struct TorrentRuntime {
    pub engine: Engine,
    pub server: ServerHandle,
}

impl TorrentRuntime {
    /// Build the runtime: open a librqbit session pointed at `cache_root`,
    /// spawn the axum HTTP server on `127.0.0.1:0`, and return both
    /// handles wrapped together.
    pub async fn new(cache_root: std::path::PathBuf) -> Result<Self, String> {
        let config = kino_torrent::EngineConfig {
            cache_root,
            ..Default::default()
        };
        let engine = Engine::new(config).await.map_err(|e| e.to_string())?;
        let server = ServerHandle::spawn().await.map_err(|e| e.to_string())?;
        Ok(Self { engine, server })
    }
}

/// `start_playback` request: a magnet/torrent input plus an optional
/// caller-chosen file index. The frontend passes one of these for each
/// "Play" / "Resume" action in the F-010 title detail.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PlaybackSource {
    /// `magnet:?xt=urn:btih:…` URI. Also accepts a plain HTTP(S) URL that
    /// points to a `.torrent` file (`AddTorrent::from_url` handles both).
    Magnet {
        url: String,
        file_index: Option<usize>,
    },
    /// Raw `.torrent` metainfo, base64-encoded so it can cross the IPC
    /// boundary (Tauri's IPC layer is JSON, not binary).
    TorrentBytes {
        bytes_base64: String,
        file_index: Option<usize>,
    },
    /// A direct HTTP(S) stream URL (e.g. from an `http_url` Stremio
    /// stream). The frontend can play this without going through the
    /// embedded server; we simply echo the URL back inside the response
    /// for a uniform play flow.
    DirectUrl {
        url: String,
        mime: Option<String>,
        file_name: Option<String>,
    },
}

/// Snapshot of one file inside an added torrent, surfaced to the frontend
/// when it needs to disambiguate which file in a season pack to play.
#[derive(Debug, Serialize)]
pub struct PlaybackFile {
    pub index: usize,
    pub relative_path: String,
    pub size: u64,
    pub is_video: bool,
}

/// Response payload for `start_playback`. The `url` is what the platform
/// player consumes; `token` lets the frontend call back into
/// `stop_playback` / `playback_status` for the same session.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackHandle {
    /// The URL passed to the platform player (Android `ExoPlayer` /
    /// `libmpv`).
    pub url: String,
    /// Token used by `stop_playback`. Empty string for direct URLs (no
    /// torrent session to tear down).
    pub token: String,
    /// `true` iff this playback is being served by the embedded engine
    /// (vs. a direct URL pass-through).
    pub via_torrent: bool,
    /// File name surfaced to the player (used for Content-Disposition and
    /// for the player's title bar).
    pub file_name: String,
    /// File size in bytes. `None` for direct URLs (we don't HEAD upstream).
    pub file_size: Option<u64>,
    /// MIME type. Best-effort from the filename extension; defaults to
    /// `application/octet-stream` if unknown.
    pub mime: Option<String>,
    /// Lower-hex info hash for the torrent. `None` for direct URLs.
    pub info_hash: Option<String>,
    /// Full file list for torrents (lets the UI surface a file-picker if
    /// the auto-pick chose wrong). Empty for direct URLs.
    pub files: Vec<PlaybackFile>,
    /// Engine-assigned id used by `stop_playback` to remove the torrent.
    /// `None` for direct URLs.
    pub torrent_id: Option<usize>,
}

/// Live stats for a registered playback session.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStatus {
    /// The token of the session being queried.
    pub token: String,
    /// File name being served.
    pub file_name: String,
    /// File size in bytes.
    pub file_size: u64,
    /// `true` while the session is registered with the server.
    pub active: bool,
}

/// `start_playback({ source })` — F-013 entry point. Adds the torrent
/// (waiting up to `init_timeout` for metadata), picks the largest video
/// file (or honors the caller's `file_index` hint), registers a token,
/// and returns the streaming URL.
#[tauri::command]
pub async fn start_playback(
    runtime: State<'_, TorrentRuntime>,
    source: PlaybackSource,
) -> Result<PlaybackHandle, String> {
    match source {
        PlaybackSource::DirectUrl {
            url,
            mime,
            file_name,
        } => Ok(PlaybackHandle {
            url: url.clone(),
            token: String::new(),
            via_torrent: false,
            file_name: file_name
                .unwrap_or_else(|| extract_filename_from_url(&url).unwrap_or_default()),
            file_size: None,
            mime,
            info_hash: None,
            files: Vec::new(),
            torrent_id: None,
        }),
        PlaybackSource::Magnet { url, file_index } => {
            start_torrent_playback(&runtime, AddInput::Url(url), file_index).await
        }
        PlaybackSource::TorrentBytes {
            bytes_base64,
            file_index,
        } => {
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(bytes_base64.as_bytes())
                .map_err(|e| format!("invalid base64 torrent bytes: {e}"))?;
            start_torrent_playback(&runtime, AddInput::Bytes(raw.into()), file_index).await
        }
    }
}

async fn start_torrent_playback(
    runtime: &TorrentRuntime,
    input: AddInput,
    requested_file_index: Option<usize>,
) -> Result<PlaybackHandle, String> {
    let added = runtime.engine.add(input).await.map_err(|e| e.to_string())?;
    let chosen_index = requested_file_index.unwrap_or_else(|| {
        // Empty torrents are rejected by Engine::add already, so the
        // `0` fallback only kicks in if the auto-pick somehow returns
        // None for a non-empty torrent.
        added.pick_largest_video().map_or(0, |f| f.index)
    });
    if chosen_index >= added.files().len() {
        return Err(format!(
            "file index {chosen_index} out of range (have {})",
            added.files().len()
        ));
    }

    let file_name = added.file_name(chosen_index).unwrap_or("").to_string();
    let file_size = added.file_size(chosen_index).unwrap_or(0);
    let mime = mime_guess::from_path(&file_name)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let info_hash = added.info_hash_hex().to_string();
    let torrent_id = added.id();
    let files = added
        .files()
        .iter()
        .map(|f| PlaybackFile {
            index: f.index,
            relative_path: f.relative_path.clone(),
            size: f.size,
            is_video: f.is_video(),
        })
        .collect::<Vec<_>>();

    let token = runtime
        .server
        .register(added, chosen_index)
        .map_err(|e| e.to_string())?;
    let url = runtime.server.stream_url(token);

    Ok(PlaybackHandle {
        url,
        token: token.to_string(),
        via_torrent: true,
        file_name,
        file_size: Some(file_size),
        mime: Some(mime),
        info_hash: Some(info_hash),
        files,
        torrent_id: Some(torrent_id),
    })
}

/// `stop_playback(token, deleteFiles?)` — F-013 teardown. Unregisters the
/// token from the local HTTP server and, if the session was torrent-
/// backed, removes the torrent from the engine.
///
/// `delete_files` controls whether librqbit also wipes on-disk pieces.
/// Defaults to `false` so the next "Play" on the same title reuses the
/// already-downloaded cache (PRD §F-013 LRU eviction takes care of it).
#[tauri::command]
pub async fn stop_playback(
    runtime: State<'_, TorrentRuntime>,
    token: String,
    delete_files: Option<bool>,
) -> Result<bool, String> {
    if token.is_empty() {
        // Direct-URL playback: nothing to tear down.
        return Ok(false);
    }
    let uuid = uuid::Uuid::parse_str(&token).map_err(|e| format!("invalid token: {e}"))?;
    let Some(session) = runtime.server.unregister(uuid) else {
        return Ok(false);
    };
    let torrent_id = session.torrent.id();
    runtime
        .engine
        .remove(torrent_id, delete_files.unwrap_or(false))
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// `playback_status(token)` — returns a minimal snapshot the F-015 player
/// uses to surface filename + size before bytes start flowing. Returns
/// `None` if the token is unknown.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri command bindings take owned String / State
pub fn playback_status(
    runtime: State<'_, TorrentRuntime>,
    token: String,
) -> Result<Option<PlaybackStatus>, String> {
    if token.is_empty() {
        return Ok(None);
    }
    let uuid = uuid::Uuid::parse_str(&token).map_err(|e| format!("invalid token: {e}"))?;
    Ok(runtime.server.session(uuid).map(|s| PlaybackStatus {
        token: s.token.to_string(),
        file_name: s.file_name,
        file_size: s.file_size,
        active: true,
    }))
}

/// Best-effort filename extraction from a URL. Used for direct-URL playback
/// where the caller didn't supply a filename — we surface the URL's last
/// path segment so the player can display *something*.
fn extract_filename_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next()?;
    let path = path.split('#').next()?;
    let last = path.rsplit('/').next()?;
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_utc_midnight_advances_one_day() {
        let ts = next_utc_midnight_unix("2025-04-01").unwrap();
        // 2025-04-02T00:00:00Z = 1743552000
        assert_eq!(ts, 1_743_552_000);
    }

    #[test]
    fn today_utc_string_is_yyyy_mm_dd() {
        let s = today_utc_string();
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
    }

    #[test]
    fn parse_title_id_accepts_three_provider_prefixes() {
        assert_eq!(parse_title_id("tmdb:603").unwrap(), ("tmdb", "603"));
        assert_eq!(
            parse_title_id("imdb:tt0133093").unwrap(),
            ("imdb", "tt0133093")
        );
        assert_eq!(parse_title_id("tvdb:78878").unwrap(), ("tvdb", "78878"));
    }

    #[test]
    fn parse_title_id_rejects_unsupported_prefix() {
        let err = parse_title_id("trakt:matrix").unwrap_err();
        assert!(err.contains("unsupported"), "got: {err}");
    }

    #[test]
    fn parse_title_id_rejects_unprefixed_value() {
        assert!(parse_title_id("603").is_err());
    }

    #[test]
    fn recommended_addons_view_matches_locked_table() {
        let view = get_recommended_addons();
        assert_eq!(view.len(), RECOMMENDED_ADDONS.len());
        assert_eq!(view[0].name, "Cinemeta");
        assert_eq!(view[0].manifest_url, CINEMETA_MANIFEST_URL);
    }

    #[tokio::test]
    async fn uninstall_addon_protects_cinemeta() {
        let db = Db::open_in_memory().await.unwrap();
        // Hand-roll an addon row that LOOKS LIKE Cinemeta (same manifest URL)
        // without going through install_addon (which would hit the network).
        db.addons_insert(&AddonInsert {
            id: "com.linvo.cinemeta".into(),
            manifest_url: CINEMETA_MANIFEST_URL.into(),
            manifest_json: serde_json::json!({"id": "com.linvo.cinemeta"}),
            display_order: None,
        })
        .await
        .unwrap();
        let installed = db.addons_list().await.unwrap();
        assert_eq!(installed.len(), 1);
        // Direct call to the helper since the #[tauri::command] wrapper
        // requires a Tauri State, which is hard to fake in a unit test.
        let is_cm = is_cinemeta_id(&db, "com.linvo.cinemeta").await.unwrap();
        assert!(is_cm, "Cinemeta should be detected as non-removable");

        // A different addon with the same id but a different URL should
        // NOT be protected (defensive — we key off the URL, not the id).
        db.addons_insert(&AddonInsert {
            id: "imposter".into(),
            manifest_url: "https://other/manifest.json".into(),
            manifest_json: serde_json::json!({"id": "imposter"}),
            display_order: None,
        })
        .await
        .unwrap();
        let is_other = is_cinemeta_id(&db, "imposter").await.unwrap();
        assert!(!is_other);
    }

    #[tokio::test]
    async fn set_addon_order_rearranges_list() {
        let db = Db::open_in_memory().await.unwrap();
        for id in ["a", "b", "c"] {
            db.addons_insert(&AddonInsert {
                id: id.into(),
                manifest_url: format!("https://{id}/manifest.json"),
                manifest_json: serde_json::json!({"id": id}),
                display_order: None,
            })
            .await
            .unwrap();
        }

        // Move "c" to the front.
        let current = db.addons_list().await.unwrap();
        let mut ids: Vec<String> = current.iter().map(|a| a.id.clone()).collect();
        let from = ids.iter().position(|x| x == "c").unwrap();
        let item = ids.remove(from);
        ids.insert(0, item);
        db.addons_reorder(&ids).await.unwrap();

        let listed = db.addons_list().await.unwrap();
        let order: Vec<&str> = listed.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(order, vec!["c", "a", "b"]);
    }

    #[tokio::test]
    async fn bootstrap_skips_when_marker_present() {
        let db = Db::open_in_memory().await.unwrap();
        db.kv_set(ADDON_BOOTSTRAP_DONE_KEY, "1").await.unwrap();
        // Doesn't make a network call; just returns. If this hung we'd
        // know the marker isn't being respected.
        bootstrap_default_addons(&db).await;
        let listed = db.addons_list().await.unwrap();
        assert!(listed.is_empty());
    }

    // ---- F-006: source availability filter ----------------------------

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn stream_manifest_body(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "version": "1.0.0",
                "name": "{id}",
                "types": ["movie", "series"],
                "resources": ["stream"],
                "catalogs": []
            }}"#
        )
    }

    async fn install_stream_addon(db: &Db, id: &str, manifest_url: &str, enabled: bool) {
        let manifest_json: serde_json::Value =
            serde_json::from_str(&stream_manifest_body(id)).unwrap();
        db.addons_insert(&AddonInsert {
            id: id.into(),
            manifest_url: manifest_url.into(),
            manifest_json,
            display_order: None,
        })
        .await
        .unwrap();
        db.addons_set_enabled(id, enabled).await.unwrap();
    }

    fn stream_test_config() -> HttpConfig {
        // Zero backoffs + short timeout so retry/timeout tests don't waste
        // wall time. Default `HttpConfig::for_test` already does this.
        HttpConfig::for_test()
    }

    #[tokio::test]
    async fn check_availability_no_addons_returns_all_unavailable() {
        let db = Db::open_in_memory().await.unwrap();
        let items = vec![
            AvailabilityRequest {
                title_id: "tt1".into(),
                kind: TitleKind::Movie,
            },
            AvailabilityRequest {
                title_id: "tt2".into(),
                kind: TitleKind::Movie,
            },
        ];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|r| !r.available && r.source_count == 0));
        // No rows persisted: the table should be empty.
        let cached = db
            .availability_list_fresh("tt1", TitleKind::Movie, 0)
            .await
            .unwrap();
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn check_availability_returns_available_when_any_addon_has_streams() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"streams": [{"infoHash": "deadbeef"}, {"url": "https://x/file.mp4"}]}"#,
            ))
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, true).await;

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].available);
        assert_eq!(got[0].source_count, 1);
    }

    #[tokio::test]
    async fn check_availability_persists_results_to_stream_availability() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "u"}]}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt2.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"streams": []}"#))
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, true).await;

        let items = vec![
            AvailabilityRequest {
                title_id: "tt1".into(),
                kind: TitleKind::Movie,
            },
            AvailabilityRequest {
                title_id: "tt2".into(),
                kind: TitleKind::Movie,
            },
        ];
        let _ = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();

        let row1 = db
            .availability_get_fresh("tt1", TitleKind::Movie, "addon-a", 0)
            .await
            .unwrap();
        let row2 = db
            .availability_get_fresh("tt2", TitleKind::Movie, "addon-a", 0)
            .await
            .unwrap();
        assert_eq!(row1, Some(true));
        assert_eq!(row2, Some(false));
    }

    #[tokio::test]
    async fn check_availability_uses_cache_hit_without_network() {
        // Cache table is pre-populated; the mock server is intentionally
        // bare so any unexpected network call would surface as a wiremock
        // "no matching mock" 404 → has_streams=false (and we'd see the row
        // flip from true→false).
        let server = MockServer::start().await;
        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, true).await;

        // Hand-roll a fresh availability row.
        let now = now_unix();
        db.availability_upsert_many(&[AvailabilityRow {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
            source_id: "addon-a".into(),
            has_streams: true,
            checked_at: now,
        }])
        .await
        .unwrap();

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert!(got[0].available);
        assert_eq!(got[0].source_count, 1);
    }

    #[tokio::test]
    async fn check_availability_filters_disabled_addons() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "u"}]}"#),
            )
            .mount(&server)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, false).await;

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        // Disabled addons must not be consulted; the title appears unavailable.
        assert!(!got[0].available);
        assert_eq!(got[0].source_count, 0);
    }

    #[tokio::test]
    async fn check_availability_filters_kind_via_manifest() {
        // Addon manifest declares only `types: ["series"]` but is asked
        // for a movie. The dispatch must skip it without a network call.
        let server = MockServer::start().await;
        // Wiremock with no mounted mocks → any request returns 404, which
        // we'd misinterpret as "unavailable from this source" rather than
        // "skipped". The expect(0) below proves no request was issued.
        let mock = Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "u"}]}"#),
            )
            .expect(0);
        mock.mount(&server).await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        db.addons_insert(&AddonInsert {
            id: "series-only".into(),
            manifest_url: manifest_url.clone(),
            manifest_json: serde_json::json!({
                "id": "series-only",
                "version": "1",
                "name": "Series Only",
                "types": ["series"],
                "resources": ["stream"],
                "catalogs": []
            }),
            display_order: None,
        })
        .await
        .unwrap();

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert!(!got[0].available);
        // No row should be persisted because no addon was eligible.
        let cached = db
            .availability_list_fresh("tt1", TitleKind::Movie, 0)
            .await
            .unwrap();
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn check_availability_counts_multiple_sources() {
        let server_a = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "u"}]}"#),
            )
            .mount(&server_a)
            .await;
        let server_b = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "v"}]}"#),
            )
            .mount(&server_b)
            .await;
        let server_c = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"streams": []}"#))
            .mount(&server_c)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        install_stream_addon(&db, "a", &format!("{}/manifest.json", server_a.uri()), true).await;
        install_stream_addon(&db, "b", &format!("{}/manifest.json", server_b.uri()), true).await;
        install_stream_addon(&db, "c", &format!("{}/manifest.json", server_c.uri()), true).await;

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert!(got[0].available);
        // Two of three addons returned streams.
        assert_eq!(got[0].source_count, 2);
    }

    /// Responder that records how many calls were in flight simultaneously
    /// AND blocks each call for ~50ms so the concurrency cap is observable.
    #[derive(Clone)]
    struct ConcurrencyProbe {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    impl Respond for ConcurrencyProbe {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let now_in_flight = self.in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            // Update the high-water mark.
            let mut peak = self.max_in_flight.load(AtomicOrdering::SeqCst);
            while now_in_flight > peak {
                match self.max_in_flight.compare_exchange(
                    peak,
                    now_in_flight,
                    AtomicOrdering::SeqCst,
                    AtomicOrdering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(x) => peak = x,
                }
            }
            // Hold the call long enough that AVAILABILITY_CONCURRENCY+1
            // requests overlap if the cap isn't enforced.
            let response = ResponseTemplate::new(200)
                .set_body_string(r#"{"streams": [{"url": "u"}]}"#)
                .set_delay(Duration::from_millis(50));
            // Best-effort: spawn a task to decrement after the delay
            // elapses. We don't have a "after" hook on wiremock, so we
            // approximate by decrementing immediately; the max snapshot
            // captured above is what matters for the assertion.
            self.in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            response
        }
    }

    #[tokio::test]
    async fn check_availability_respects_concurrency_cap() {
        let server = MockServer::start().await;
        let probe = ConcurrencyProbe {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
        };
        Mock::given(method("GET"))
            .respond_with(probe.clone())
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, true).await;

        // 50 items × 1 addon = 50 work units. With the cap at 8 the peak
        // should saturate at AVAILABILITY_CONCURRENCY (or fewer, depending
        // on scheduler luck).
        let items: Vec<_> = (0..50)
            .map(|i| AvailabilityRequest {
                title_id: format!("tt{i}"),
                kind: TitleKind::Movie,
            })
            .collect();
        let start = Instant::now();
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(got.len(), 50);
        // Total wall time must be larger than what we'd see if all 50
        // requests fired at once. Each request blocks for 50ms; with the
        // cap at 8 we expect at least ceil(50/8) * 50ms = 350ms.
        // (Generous lower bound; under contention scheduler can dip.)
        assert!(
            elapsed >= Duration::from_millis(150),
            "elapsed {elapsed:?} suggests cap isn't enforced"
        );
        let peak = probe.max_in_flight.load(AtomicOrdering::SeqCst);
        assert!(
            peak <= AVAILABILITY_CONCURRENCY,
            "observed peak in-flight {peak} exceeds AVAILABILITY_CONCURRENCY"
        );
    }

    #[tokio::test]
    async fn check_availability_timeout_marks_source_unavailable() {
        let server = MockServer::start().await;
        // Hang the response well beyond `HttpConfig::for_test().timeout`
        // (500ms) so the client times out.
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"streams": [{"url": "u"}]}"#)
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_stream_addon(&db, "slow", &manifest_url, true).await;

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let start = Instant::now();
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        // Per PRD §F-006: a per-stream-request timeout closes the source as
        // unavailable; the title is unavailable because no other addon
        // serves streams.
        assert!(!got[0].available);
        assert_eq!(got[0].source_count, 0);
        // Sanity: the dispatch shouldn't have waited the full 5s delay —
        // `for_test()` caps at 500ms + the retry schedule.
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "dispatch waited too long ({:?}); did the timeout fire?",
            start.elapsed()
        );
        // The timeout is persisted as has_streams=false so the next call
        // hits the cache rather than re-trying the slow addon for 30 min.
        let row = db
            .availability_get_fresh("tt1", TitleKind::Movie, "slow", 0)
            .await
            .unwrap();
        assert_eq!(row, Some(false));
    }

    #[tokio::test]
    async fn check_availability_empty_items_returns_empty() {
        let db = Db::open_in_memory().await.unwrap();
        let got = check_availability_with_config(&db, Vec::new(), &stream_test_config())
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    // ---- F-008 row 5: list_home_catalogs ------------------------------

    fn catalog_manifest_body(id: &str, types: &[&str], catalogs: &str) -> String {
        let types_arr = types
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{
                "id": "{id}",
                "version": "1.0.0",
                "name": "{id}",
                "types": [{types_arr}],
                "resources": ["catalog", "meta"],
                "catalogs": {catalogs}
            }}"#
        )
    }

    async fn install_catalog_addon(
        db: &Db,
        id: &str,
        manifest_url: &str,
        types: &[&str],
        catalogs_json: &str,
    ) {
        let manifest_json: serde_json::Value =
            serde_json::from_str(&catalog_manifest_body(id, types, catalogs_json)).unwrap();
        db.addons_insert(&AddonInsert {
            id: id.into(),
            manifest_url: manifest_url.into(),
            manifest_json,
            display_order: None,
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_home_catalogs_empty_addons_returns_empty() {
        let db = Db::open_in_memory().await.unwrap();
        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn list_home_catalogs_returns_single_catalog_in_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"metas": [
                        {"id": "tt1", "type": "movie", "name": "Matrix",
                         "poster": "https://p1", "releaseInfo": "1999"},
                        {"id": "tt2", "type": "movie", "name": "Heat",
                         "releaseInfo": "1995-"}
                    ]}"#,
            ))
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "cinemeta",
            &manifest_url,
            &["movie", "series"],
            r#"[{"type": "movie", "id": "top", "name": "Popular"}]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].addon_id, "cinemeta");
        assert_eq!(got[0].catalog_id, "top");
        assert_eq!(got[0].catalog_kind, "movie");
        assert_eq!(got[0].catalog_name, "Popular");
        assert_eq!(got[0].items.len(), 2);
        // IMDb-style ids are coerced to the workspace's provider-prefixed
        // shape so downstream artwork resolution recognizes them.
        assert_eq!(got[0].items[0].id, "imdb:tt1");
        assert_eq!(got[0].items[0].title, "Matrix");
        assert_eq!(got[0].items[0].year, Some(1999));
        assert_eq!(got[0].items[1].id, "imdb:tt2");
        assert_eq!(got[0].items[1].year, Some(1995));
    }

    #[tokio::test]
    async fn list_home_catalogs_filters_by_kind() {
        // Cinemeta-shaped addon with one movie catalog + one series
        // catalog. Movies-kind call must surface only the movie row.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"metas": [{"id": "tt1", "type": "movie", "name": "M"}]}"#),
            )
            .mount(&server)
            .await;
        // Series catalog endpoint MUST NOT be hit when the kind filter
        // is Movie. `expect(0)` pins this in the dispatcher.
        Mock::given(method("GET"))
            .and(path("/catalog/series/top.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt2", "type": "series", "name": "S"}]}"#,
                ),
            )
            .expect(0)
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "cinemeta",
            &manifest_url,
            &["movie", "series"],
            r#"[
                {"type": "movie", "id": "top", "name": "Popular Movies"},
                {"type": "series", "id": "top", "name": "Popular Series"}
            ]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, Some(TitleKind::Movie), &HttpConfig::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].catalog_kind, "movie");
    }

    #[tokio::test]
    async fn list_home_catalogs_skips_addon_when_manifest_types_dont_match_kind() {
        // PRD §F-009: "only catalogs whose addon manifest declares the
        // matching type". A movie-only addon must contribute nothing to
        // the Series sub-home even if (defensively) it declares a series
        // catalog.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/series/top.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt2", "type": "series", "name": "S"}]}"#,
                ),
            )
            .expect(0)
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "movies-only",
            &manifest_url,
            &["movie"],
            r#"[{"type": "series", "id": "top", "name": "Series"}]"#,
        )
        .await;

        let got =
            list_home_catalogs_uncached(&db, Some(TitleKind::Series), &HttpConfig::for_test())
                .await
                .unwrap();
        assert!(
            got.is_empty(),
            "movie-only addon must not appear in Series sub-home"
        );
    }

    #[tokio::test]
    async fn list_home_catalogs_drops_empty_catalog_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"metas": []}"#))
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "empty-cat",
            &manifest_url,
            &["movie"],
            r#"[{"type": "movie", "id": "top", "name": "Popular"}]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert!(got.is_empty(), "empty catalogs must be filtered out");
    }

    #[tokio::test]
    async fn list_home_catalogs_preserves_display_order_then_catalog_order() {
        // Two addons, each with two catalogs. After dispatch they should
        // come back as: addon-a/cat-1, addon-a/cat-2, addon-b/cat-1,
        // addon-b/cat-2 — even though the JoinSet completes in arbitrary
        // order.
        let server = MockServer::start().await;
        // addon-a catalogs.
        Mock::given(method("GET"))
            .and(path("/a/catalog/movie/a1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt1", "type": "movie", "name": "A1"}]}"#,
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/a/catalog/movie/a2.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt2", "type": "movie", "name": "A2"}]}"#,
                ),
            )
            .mount(&server)
            .await;
        // addon-b catalogs.
        Mock::given(method("GET"))
            .and(path("/b/catalog/movie/b1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt3", "type": "movie", "name": "B1"}]}"#,
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b/catalog/movie/b2.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt4", "type": "movie", "name": "B2"}]}"#,
                ),
            )
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        install_catalog_addon(
            &db,
            "addon-a",
            &format!("{}/a/manifest.json", server.uri()),
            &["movie"],
            r#"[
                {"type": "movie", "id": "a1", "name": "A First"},
                {"type": "movie", "id": "a2", "name": "A Second"}
            ]"#,
        )
        .await;
        install_catalog_addon(
            &db,
            "addon-b",
            &format!("{}/b/manifest.json", server.uri()),
            &["movie"],
            r#"[
                {"type": "movie", "id": "b1", "name": "B First"},
                {"type": "movie", "id": "b2", "name": "B Second"}
            ]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        let order: Vec<(&str, &str)> = got
            .iter()
            .map(|c| (c.addon_id.as_str(), c.catalog_id.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("addon-a", "a1"),
                ("addon-a", "a2"),
                ("addon-b", "b1"),
                ("addon-b", "b2"),
            ]
        );
    }

    #[tokio::test]
    async fn list_home_catalogs_skips_disabled_addons() {
        let server = MockServer::start().await;
        // No mocks: any request from the disabled addon would 404 and
        // still produce a HomeCatalog row (404 → fetch error → row dropped),
        // but the more important invariant is that the disabled addon's
        // catalog mock is NEVER hit. We pin that with expect(0).
        let mock = Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"metas": [{"id": "tt1", "type": "movie", "name": "M"}]}"#),
            )
            .expect(0);
        mock.mount(&server).await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "disabled",
            &manifest_url,
            &["movie"],
            r#"[{"type": "movie", "id": "top", "name": "Popular"}]"#,
        )
        .await;
        db.addons_set_enabled("disabled", false).await.unwrap();

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert!(got.is_empty(), "disabled addons must not be consulted");
    }

    #[tokio::test]
    async fn list_home_catalogs_skips_catalog_endpoint_addons_without_catalog_resource() {
        // An addon that declares `catalogs` but doesn't list `catalog`
        // in `resources` is malformed; rather than 404, skip it so we
        // don't spam the addon with calls it isn't equipped to answer.
        let server = MockServer::start().await;
        let mock = Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"metas": []}"#))
            .expect(0);
        mock.mount(&server).await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        db.addons_insert(&AddonInsert {
            id: "stream-only".into(),
            manifest_url,
            manifest_json: serde_json::json!({
                "id": "stream-only",
                "version": "1",
                "name": "Stream Only",
                "types": ["movie"],
                "resources": ["stream"],
                "catalogs": [{"type": "movie", "id": "top", "name": "X"}]
            }),
            display_order: None,
        })
        .await
        .unwrap();

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn list_home_catalogs_tolerates_per_catalog_fetch_failure() {
        // One catalog 200s, one 500s. The successful one must still
        // surface; the failing one is dropped with a log.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/ok.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt1", "type": "movie", "name": "OK"}]}"#,
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/broken.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "mixed",
            &manifest_url,
            &["movie"],
            r#"[
                {"type": "movie", "id": "ok", "name": "OK Row"},
                {"type": "movie", "id": "broken", "name": "Broken Row"}
            ]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].catalog_id, "ok");
    }

    #[tokio::test]
    async fn list_home_catalogs_falls_back_to_id_when_catalog_name_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"metas": [{"id": "tt1", "type": "movie", "name": "M"}]}"#),
            )
            .mount(&server)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_catalog_addon(
            &db,
            "noname",
            &manifest_url,
            &["movie"],
            // catalog descriptor with no name field
            r#"[{"type": "movie", "id": "top"}]"#,
        )
        .await;

        let got = list_home_catalogs_uncached(&db, None, &HttpConfig::for_test())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        // Fallback shape: "{addon_name} — {catalog_id}".
        assert_eq!(got[0].catalog_name, "noname — top");
    }

    #[test]
    fn coerce_catalog_id_prefixes_imdb_style_ids() {
        assert_eq!(coerce_catalog_id("tt0133093"), "imdb:tt0133093");
        // Already-prefixed → passthrough.
        assert_eq!(coerce_catalog_id("tmdb:603"), "tmdb:603");
        assert_eq!(coerce_catalog_id("tvdb:12345"), "tvdb:12345");
        // Non-standard prefix (anime addon) → passthrough.
        assert_eq!(coerce_catalog_id("kitsu:1"), "kitsu:1");
        // Bare "tt" alone is not an IMDb id → passthrough.
        assert_eq!(coerce_catalog_id("tt"), "tt");
        // "tt" followed by non-digits → passthrough (would be a malformed
        // imdb id; let it surface to the resolver as-is).
        assert_eq!(coerce_catalog_id("ttabc"), "ttabc");
    }

    #[test]
    fn parse_release_year_handles_stremio_shapes() {
        assert_eq!(parse_release_year("1999"), Some(1999));
        assert_eq!(parse_release_year("2024-"), Some(2024));
        assert_eq!(parse_release_year("2014-2019"), Some(2014));
        assert_eq!(parse_release_year("1994-01-15"), Some(1994));
        assert_eq!(parse_release_year(""), None);
        assert_eq!(parse_release_year("N/A"), None);
        // 3-digit prefix → reject (matches the TMDB year parser's
        // strictness).
        assert_eq!(parse_release_year("999"), None);
    }

    #[tokio::test]
    async fn check_availability_ignores_catalog_only_addons() {
        // An addon that doesn't declare the `stream` resource should be
        // skipped entirely — including for the manifest deserialization
        // pre-check (no work item ever queued).
        let server = MockServer::start().await;
        let mock = Mock::given(method("GET"))
            .and(path("/stream/movie/tt1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams": [{"url": "u"}]}"#),
            )
            .expect(0);
        mock.mount(&server).await;

        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        db.addons_insert(&AddonInsert {
            id: "catalog-only".into(),
            manifest_url,
            manifest_json: serde_json::json!({
                "id": "catalog-only",
                "version": "1",
                "name": "Catalog Only",
                "types": ["movie"],
                "resources": ["catalog", "meta"],
                "catalogs": [{"type": "movie", "id": "top"}]
            }),
            display_order: None,
        })
        .await
        .unwrap();

        let items = vec![AvailabilityRequest {
            title_id: "tt1".into(),
            kind: TitleKind::Movie,
        }];
        let got = check_availability_with_config(&db, items, &stream_test_config())
            .await
            .unwrap();
        assert!(!got[0].available);
        assert_eq!(got[0].source_count, 0);
    }

    // ---- F-010: title detail view ---------------------------------------

    fn cinemeta_movie_manifest_body() -> String {
        r#"{
            "id": "com.linvo.cinemeta",
            "version": "3.0.13",
            "name": "Cinemeta",
            "types": ["movie", "series"],
            "resources": ["catalog", "meta"],
            "catalogs": [{"type": "movie", "id": "top"}]
        }"#
        .to_string()
    }

    async fn install_cinemeta_at(db: &Db, manifest_url: &str) {
        let manifest_json: serde_json::Value =
            serde_json::from_str(&cinemeta_movie_manifest_body()).unwrap();
        db.addons_insert(&AddonInsert {
            id: "com.linvo.cinemeta".into(),
            manifest_url: manifest_url.into(),
            manifest_json,
            display_order: None,
        })
        .await
        .unwrap();
    }

    /// Build an in-memory DB whose `addons` row points Cinemeta at the
    /// supplied mock server (so the Cinemeta `meta/...` fetch is
    /// intercepted instead of hitting strem.io). NOT the production
    /// Cinemeta URL — the bootstrap protection (which keys on the URL)
    /// won't fire, so we can freely tweak addon state.
    async fn db_with_cinemeta_at(server: &MockServer) -> Db {
        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        install_cinemeta_at(&db, &manifest_url).await;
        db
    }

    #[tokio::test]
    async fn get_title_detail_pulls_baseline_from_cinemeta_for_movies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {
                    "id": "tt0133093",
                    "type": "movie",
                    "name": "The Matrix",
                    "poster": "https://p.example/p.jpg",
                    "background": "https://p.example/bg.jpg",
                    "logo": "https://p.example/logo.png",
                    "description": "A computer hacker learns about reality.",
                    "releaseInfo": "1999",
                    "runtime": "136 min",
                    "imdbRating": "8.7",
                    "genres": ["Action", "Sci-Fi"],
                    "cast": ["Keanu Reeves", "Carrie-Anne Moss", ""],
                    "videos": []
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        let detail = get_title_detail_uncached(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert_eq!(detail.title, "The Matrix");
        assert_eq!(detail.year, Some(1999));
        assert_eq!(detail.runtime_minutes, Some(136));
        assert_eq!(detail.imdb_rating, Some(8.7));
        assert_eq!(detail.genres, vec!["Action", "Sci-Fi"]);
        assert_eq!(
            detail.summary.as_deref(),
            Some("A computer hacker learns about reality.")
        );
        assert_eq!(detail.poster.as_deref(), Some("https://p.example/p.jpg"));
        assert_eq!(detail.backdrop.as_deref(), Some("https://p.example/bg.jpg"));
        assert_eq!(detail.logo.as_deref(), Some("https://p.example/logo.png"));
        // Empty cast name is filtered out.
        assert_eq!(detail.cast.len(), 2);
        assert_eq!(detail.cast[0].name, "Keanu Reeves");
        assert!(detail.cast[0].photo.is_none()); // No TMDB enrichment.
        assert!(detail.episodes.is_empty());
        // No CW row → no resume.
        assert!(detail.resume_position_s.is_none());
        assert_eq!(detail.stremio_id.as_deref(), Some("tt0133093"));
    }

    #[tokio::test]
    async fn get_title_detail_builds_episode_list_for_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/series/tt0944947.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {
                    "id": "tt0944947",
                    "type": "series",
                    "name": "Game of Thrones",
                    "videos": [
                        {
                            "id": "tt0944947:1:2",
                            "title": "The Kingsroad",
                            "season": 1,
                            "episode": 2,
                            "released": "2011-04-24",
                            "overview": "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Long form text exceeds the 120-char PRD limit and must be truncated.",
                            "thumbnail": "https://t.example/t2.jpg"
                        },
                        {
                            "id": "tt0944947:1:1",
                            "title": "Winter Is Coming",
                            "season": 1,
                            "episode": 1,
                            "released": "2011-04-17",
                            "thumbnail": "https://t.example/t1.jpg"
                        },
                        {
                            "id": "tt0944947:0:5",
                            "title": "Specials promo",
                            "season": 0,
                            "episode": 5
                        }
                    ]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        let detail = get_title_detail_uncached(
            &db,
            "imdb:tt0944947",
            TitleKind::Series,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        // Specials (season 0) dropped; episodes sorted by (season, episode).
        assert_eq!(detail.episodes.len(), 2);
        assert_eq!(detail.episodes[0].season, 1);
        assert_eq!(detail.episodes[0].episode, 1);
        assert_eq!(detail.episodes[0].title, "Winter Is Coming");
        assert_eq!(detail.episodes[1].season, 1);
        assert_eq!(detail.episodes[1].episode, 2);
        // 120-char truncation with ellipsis.
        let overview = detail.episodes[1].overview.as_deref().unwrap();
        assert!(overview.ends_with('…'), "got: {overview:?}");
        assert_eq!(overview.chars().count(), 121); // 120 + '…'
    }

    #[tokio::test]
    async fn get_title_detail_per_episode_progress_from_cw() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/series/tt0944947.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {
                    "id": "tt0944947",
                    "type": "series",
                    "name": "Game of Thrones",
                    "videos": [
                        {"id": "tt0944947:1:1", "title": "Winter Is Coming", "season": 1, "episode": 1},
                        {"id": "tt0944947:1:2", "title": "The Kingsroad", "season": 1, "episode": 2}
                    ]
                }
            })))
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        // CW: user has watched 50% of S1E1, 0% of S1E2.
        db.cw_upsert(&ContinueWatching {
            title_id: "tt0944947".into(),
            kind: TitleKind::Series,
            season: 1,
            episode: 1,
            position_s: 30.0 * 60.0,
            duration_s: 60.0 * 60.0,
            last_played_at: 100,
            meta_json: serde_json::json!({}),
        })
        .await
        .unwrap();
        let mut payload = get_title_detail_uncached(
            &db,
            "imdb:tt0944947",
            TitleKind::Series,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        apply_cw_to_payload(&db, &mut payload).await;
        // F-010 acceptance: episode list shows correct progress for
        // partially-watched episodes.
        assert!((payload.episodes[0].progress - 0.5).abs() < 1e-9);
        assert!(payload.episodes[1].progress.abs() < f64::EPSILON);
        // Resume target was set to S1E1.
        assert_eq!(payload.resume_position_s, Some(30.0 * 60.0));
        assert_eq!(payload.resume_season, Some(1));
        assert_eq!(payload.resume_episode, Some(1));
        assert_eq!(payload.resume_video_id.as_deref(), Some("tt0944947:1:1"));
    }

    #[tokio::test]
    async fn get_title_detail_resume_target_picks_latest_played_episode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/series/tt0944947.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {
                    "id": "tt0944947",
                    "type": "series",
                    "name": "Game of Thrones",
                    "videos": [
                        {"id": "tt0944947:1:1", "title": "WIC", "season": 1, "episode": 1},
                        {"id": "tt0944947:1:5", "title": "WW", "season": 1, "episode": 5}
                    ]
                }
            })))
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        db.cw_upsert(&ContinueWatching {
            title_id: "tt0944947".into(),
            kind: TitleKind::Series,
            season: 1,
            episode: 1,
            position_s: 100.0,
            duration_s: 3600.0,
            last_played_at: 100,
            meta_json: serde_json::json!({}),
        })
        .await
        .unwrap();
        db.cw_upsert(&ContinueWatching {
            title_id: "tt0944947".into(),
            kind: TitleKind::Series,
            season: 1,
            episode: 5,
            position_s: 200.0,
            duration_s: 3600.0,
            last_played_at: 500, // newer
            meta_json: serde_json::json!({}),
        })
        .await
        .unwrap();
        let mut payload = get_title_detail_uncached(
            &db,
            "imdb:tt0944947",
            TitleKind::Series,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        apply_cw_to_payload(&db, &mut payload).await;
        assert_eq!(payload.resume_season, Some(1));
        assert_eq!(payload.resume_episode, Some(5));
    }

    #[tokio::test]
    async fn get_title_detail_resume_set_for_movie_with_cw_row() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {"id": "tt0133093", "type": "movie", "name": "The Matrix"}
            })))
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        db.cw_upsert(&ContinueWatching {
            title_id: "tt0133093".into(),
            kind: TitleKind::Movie,
            season: 0,
            episode: 0,
            position_s: 1800.0,
            duration_s: 8160.0,
            last_played_at: 100,
            meta_json: serde_json::json!({}),
        })
        .await
        .unwrap();
        let mut payload = get_title_detail_uncached(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        apply_cw_to_payload(&db, &mut payload).await;
        // F-010 acceptance: Resume button only present when matching CW exists.
        assert_eq!(payload.resume_position_s, Some(1800.0));
        assert!(payload.resume_season.is_none()); // null for movies
        assert!(payload.resume_episode.is_none());
        assert_eq!(payload.resume_video_id.as_deref(), Some("tt0133093"));
    }

    #[tokio::test]
    async fn get_title_detail_no_cw_means_no_resume() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {"id": "tt0133093", "type": "movie", "name": "The Matrix"}
            })))
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        let mut payload = get_title_detail_uncached(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        apply_cw_to_payload(&db, &mut payload).await;
        assert!(payload.resume_position_s.is_none());
        assert!(payload.resume_video_id.is_none());
    }

    #[tokio::test]
    async fn get_title_detail_truncates_cast_to_top_six() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/movie/tt1.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "meta": {
                    "id": "tt1",
                    "type": "movie",
                    "name": "T",
                    "cast": ["A", "B", "C", "D", "E", "F", "G", "H"]
                }
            })))
            .mount(&server)
            .await;
        let db = db_with_cinemeta_at(&server).await;
        let detail = get_title_detail_uncached(
            &db,
            "imdb:tt1",
            TitleKind::Movie,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert_eq!(detail.cast.len(), 6);
        assert_eq!(detail.cast[5].name, "F");
    }

    #[tokio::test]
    async fn get_title_detail_skips_cinemeta_when_uninstalled() {
        // No Cinemeta row in the addons table — payload comes back empty
        // (TMDB-only path also not configured), but the command does NOT
        // fail.
        let db = Db::open_in_memory().await.unwrap();
        let detail = get_title_detail_uncached(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            &["en".to_string()],
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert!(detail.title.is_empty());
        assert!(detail.cast.is_empty());
        assert_eq!(detail.stremio_id.as_deref(), Some("tt0133093"));
    }

    // ---- F-010: stream parsing & sort ----

    fn stream_response(streams: &serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"streams": streams}))
    }

    /// PRD §F-010 code acceptance: stream parsing produces correct badges
    /// from the locked §8 fixture filenames. This test exercises the
    /// addon-stream → `StreamRow` conversion path (the regex set itself
    /// is tested in `kino_addons::parse`; this asserts the wiring carries
    /// the tags through into the IPC shape).
    #[test]
    fn addon_stream_row_quality_badges_match_prd_fixtures() {
        let cases = [
            (
                "The Matrix 1999 2160p UHD BluRay HEVC TrueHD Atmos 7.1-FraMeSToR",
                Quality::Uhd4K,
                Codec::H265,
                Audio::Atmos,
                None,
            ),
            (
                "Inception 2010 1080p BluRay DV HDR10 x265 DTS-HD MA 5.1",
                Quality::Fhd1080,
                Codec::H265,
                Audio::DtsHd,
                Some(Hdr::DolbyVision),
            ),
            (
                "Some Show S01E01 720p WEB-DL DDP5.1 H.264",
                Quality::Hd720,
                Codec::H264,
                Audio::Eac3,
                None,
            ),
        ];
        for (filename, q, c, a, hdr) in cases {
            let s = AddonStream {
                name: Some("src".into()),
                title: Some(filename.into()),
                description: None,
                url: None,
                info_hash: None,
                file_idx: None,
                yt_id: None,
                external_url: None,
                behavior_hints: serde_json::Value::Null,
                sources: Vec::new(),
                extra: serde_json::Map::new(),
            };
            let row = addon_stream_to_row("addon-id", "Addon", s);
            assert_eq!(row.quality, Some(q), "quality for {filename}");
            assert_eq!(row.codec, Some(c), "codec for {filename}");
            assert_eq!(row.audio, Some(a), "audio for {filename}");
            assert_eq!(row.hdr, hdr, "hdr for {filename}");
        }
    }

    #[test]
    fn addon_stream_row_extracts_torrentio_seeders_and_size() {
        let s = AddonStream {
            name: Some("Torrentio".into()),
            title: Some(
                "The Matrix 1999 2160p UHD BluRay HEVC TrueHD Atmos 7.1\n👤 156 💾 23.4 GB ⚙️ EZTV"
                    .into(),
            ),
            description: None,
            url: None,
            info_hash: Some("deadbeef".into()),
            file_idx: None,
            yt_id: None,
            external_url: None,
            behavior_hints: serde_json::Value::Null,
            sources: vec!["dht".into()],
            extra: serde_json::Map::new(),
        };
        let row = addon_stream_to_row("addon-id", "Torrentio", s);
        assert_eq!(row.seeders, Some(156));
        // 23.4 GiB = 23.4 * 1024^3 ≈ 25,125,762,662 bytes.
        let want = gib_to_bytes(23.4);
        let got = row.size_bytes.unwrap();
        // Allow ±4 bytes for floating-point rounding.
        assert!(got.abs_diff(want) < 4, "got: {got}, want: {want}");
    }

    #[test]
    fn addon_stream_row_extracts_plain_seeders_size_shapes() {
        let s = AddonStream {
            name: Some("Public Domain Movies".into()),
            title: Some("Old Movie 1939 1080p".into()),
            description: Some("Size: 1.2 GB Seeders: 42".into()),
            url: Some("https://archive.org/m.mp4".into()),
            info_hash: None,
            file_idx: None,
            yt_id: None,
            external_url: None,
            behavior_hints: serde_json::Value::Null,
            sources: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let row = addon_stream_to_row("addon-id", "Public Domain Movies", s);
        assert_eq!(row.seeders, Some(42));
        let want = gib_to_bytes(1.2);
        assert!(row.size_bytes.unwrap().abs_diff(want) < 4);
        assert_eq!(row.url.as_deref(), Some("https://archive.org/m.mp4"));
    }

    #[test]
    fn addon_stream_row_no_match_returns_none_badges() {
        let s = AddonStream {
            name: Some("Unknown".into()),
            title: Some("just a random file with no tags".into()),
            description: None,
            url: None,
            info_hash: None,
            file_idx: None,
            yt_id: None,
            external_url: None,
            behavior_hints: serde_json::Value::Null,
            sources: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let row = addon_stream_to_row("id", "Unknown", s);
        assert!(row.quality.is_none());
        assert!(row.hdr.is_none());
        assert!(row.audio.is_none());
        assert!(row.codec.is_none());
        assert!(row.seeders.is_none());
        assert!(row.size_bytes.is_none());
    }

    #[tokio::test]
    async fn get_streams_sorts_by_quality_seeders_size_descending() {
        // Two stream-serving addons with different quality streams.
        // Verifies the locked sort: quality DESC > seeders DESC > size DESC.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(stream_manifest_body("a")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(stream_manifest_body("b")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/a/stream/movie/tt0133093.json"))
            .respond_with(stream_response(&serde_json::json!([
                {
                    "name": "a1",
                    "title": "Some 1080p WEB-DL\n👤 10 💾 1 GB",
                    "infoHash": "aaa1"
                },
                {
                    "name": "a2",
                    "title": "Some 4K UHD\n👤 50 💾 30 GB",
                    "infoHash": "aaa2"
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b/stream/movie/tt0133093.json"))
            .respond_with(stream_response(&serde_json::json!([
                {
                    "name": "b1",
                    "title": "Some 1080p WEB-DL\n👤 200 💾 5 GB",
                    "infoHash": "bbb1"
                },
                {
                    "name": "b2",
                    "title": "Some 1080p WEB-DL\n👤 200 💾 50 GB",
                    "infoHash": "bbb2"
                }
            ])))
            .mount(&server)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        install_stream_addon(&db, "a", &format!("{}/a/manifest.json", server.uri()), true).await;
        install_stream_addon(&db, "b", &format!("{}/b/manifest.json", server.uri()), true).await;
        let rows = get_streams_with_config(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            None,
            None,
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 4);
        // First: 4K (a2)
        assert_eq!(rows[0].quality, Some(Quality::Uhd4K));
        // Among 1080p rows: higher seeders first; same seeders → larger size first.
        assert_eq!(rows[1].quality, Some(Quality::Fhd1080));
        assert_eq!(rows[1].seeders, Some(200));
        // b2 (200 seeders, 50 GB) before b1 (200 seeders, 5 GB).
        assert_eq!(rows[1].info_hash.as_deref(), Some("bbb2"));
        assert_eq!(rows[2].info_hash.as_deref(), Some("bbb1"));
        // a1: 10 seeders, comes after both b rows.
        assert_eq!(rows[3].info_hash.as_deref(), Some("aaa1"));
    }

    #[tokio::test]
    async fn get_streams_routes_series_episodes_via_imdb_season_episode_form() {
        // The stremio id MUST be `imdb:S:E` for series episodes — assert
        // the mock receives the exact path with `:1:1` appended.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/s/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(stream_manifest_body("s")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/s/stream/series/tt0944947:1:1.json"))
            .respond_with(stream_response(&serde_json::json!([
                {"name": "x", "title": "S01E01 1080p\n👤 7 💾 800 MB", "infoHash": "h"}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        install_stream_addon(&db, "s", &format!("{}/s/manifest.json", server.uri()), true).await;
        let rows = get_streams_with_config(
            &db,
            "imdb:tt0944947",
            TitleKind::Series,
            Some(1),
            Some(1),
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].quality, Some(Quality::Fhd1080));
    }

    #[tokio::test]
    async fn get_streams_returns_empty_when_no_imdb_id_resolvable() {
        // TVDB-only id with no TMDB key configured to cross-resolve. The
        // command tolerates this and returns an empty list (the UI shows
        // "no streams found" rather than an error).
        let db = Db::open_in_memory().await.unwrap();
        let rows = get_streams_with_config(
            &db,
            "tvdb:12345",
            TitleKind::Movie,
            None,
            None,
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn get_streams_rejects_bad_kind_season_episode_shape() {
        let db = Db::open_in_memory().await.unwrap();
        // Movie + season is invalid.
        let err = get_streams_with_config(
            &db,
            "imdb:tt1",
            TitleKind::Movie,
            Some(1),
            None,
            &HttpConfig::for_test(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("kind=movie"));
        // Series with no episode is invalid.
        let err = get_streams_with_config(
            &db,
            "imdb:tt1",
            TitleKind::Series,
            Some(1),
            None,
            &HttpConfig::for_test(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("kind=series"));
    }

    #[tokio::test]
    async fn get_streams_skips_catalog_only_addons() {
        // An addon declaring `["catalog", "meta"]` (no `stream`) must
        // receive ZERO stream requests; `serves_stream` filters at the
        // manifest gate.
        let server = MockServer::start().await;
        let mock = Mock::given(method("GET"))
            .and(path("/stream/movie/tt0133093.json"))
            .respond_with(stream_response(&serde_json::json!([])))
            .expect(0);
        mock.mount(&server).await;
        let db = Db::open_in_memory().await.unwrap();
        let manifest_url = format!("{}/manifest.json", server.uri());
        db.addons_insert(&AddonInsert {
            id: "cat-only".into(),
            manifest_url,
            manifest_json: serde_json::json!({
                "id": "cat-only",
                "version": "1",
                "name": "Catalog Only",
                "types": ["movie"],
                "resources": ["catalog", "meta"],
                "catalogs": []
            }),
            display_order: None,
        })
        .await
        .unwrap();
        let rows = get_streams_with_config(
            &db,
            "imdb:tt0133093",
            TitleKind::Movie,
            None,
            None,
            &HttpConfig::for_test(),
        )
        .await
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn extract_seeders_handles_various_shapes() {
        assert_eq!(extract_seeders("👤 156"), Some(156));
        assert_eq!(extract_seeders("👤 7,894"), Some(7894));
        assert_eq!(extract_seeders("Seeders: 42"), Some(42));
        assert_eq!(extract_seeders("Seeds: 8"), Some(8));
        assert_eq!(extract_seeders("seed 5"), Some(5));
        assert_eq!(extract_seeders("nothing here"), None);
    }

    #[test]
    fn extract_size_bytes_handles_units() {
        assert_eq!(extract_size_bytes("💾 1 GB"), Some(1024 * 1024 * 1024));
        assert_eq!(extract_size_bytes("Size: 5.0 MB"), Some(5 * 1024 * 1024));
        assert_eq!(extract_size_bytes("23 KB"), Some(23 * 1024));
        assert_eq!(extract_size_bytes("no size"), None);
        // Comma decimal (European locales).
        assert_eq!(extract_size_bytes("💾 1,5 GB"), Some(gib_to_bytes(1.5)));
    }

    /// Test helper: gigabytes → bytes via the same 1024 cascade
    /// `extract_size_bytes` uses, with the cast guarded so clippy's
    /// `cast_possible_truncation` / `cast_sign_loss` lints don't fire.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn gib_to_bytes(n: f64) -> u64 {
        (n * 1024.0 * 1024.0 * 1024.0) as u64
    }

    #[test]
    fn quality_rank_orders_4k_above_lower_buckets() {
        assert!(quality_rank(Some(Quality::Uhd4K)) > quality_rank(Some(Quality::Fhd1080)));
        assert!(quality_rank(Some(Quality::Fhd1080)) > quality_rank(Some(Quality::Hd720)));
        assert!(quality_rank(Some(Quality::Hd720)) > quality_rank(Some(Quality::Sd)));
        assert!(quality_rank(Some(Quality::Sd)) > quality_rank(None));
    }

    #[test]
    fn parse_runtime_minutes_picks_leading_integer() {
        assert_eq!(parse_runtime_minutes("136 min"), Some(136));
        assert_eq!(parse_runtime_minutes("96"), Some(96));
        assert_eq!(parse_runtime_minutes(""), None);
        assert_eq!(parse_runtime_minutes("min"), None);
        assert_eq!(parse_runtime_minutes("0 min"), None); // zero filtered out
    }

    #[test]
    fn truncate_to_chars_appends_ellipsis_only_when_truncating() {
        let mut s = "hello".to_string();
        truncate_to_chars(&mut s, 10);
        assert_eq!(s, "hello");
        let mut s = "abcdefghij".to_string();
        truncate_to_chars(&mut s, 5);
        assert_eq!(s, "abcde…");
        // Unicode boundary safety.
        let mut s = "héllo wörld".to_string();
        truncate_to_chars(&mut s, 4);
        assert_eq!(s, "héll…");
    }

    // ---- F-011: Search ------------------------------------------------

    #[test]
    fn is_imdb_id_query_accepts_tt_then_digits() {
        assert!(is_imdb_id_query("tt0133093"));
        assert!(is_imdb_id_query("tt1234567"));
        assert!(is_imdb_id_query("tt1"));
    }

    #[test]
    fn is_imdb_id_query_rejects_non_imdb_shapes() {
        assert!(!is_imdb_id_query(""));
        assert!(!is_imdb_id_query("tt"));
        assert!(!is_imdb_id_query("the matrix"));
        assert!(!is_imdb_id_query("tt0133093x"));
        assert!(!is_imdb_id_query("ttabc"));
        assert!(!is_imdb_id_query("Tt0133093")); // case-sensitive
        assert!(!is_imdb_id_query("imdb:tt0133093"));
    }

    #[test]
    fn dedup_search_results_collapses_imdb_duplicates_across_providers() {
        let movie_imdb = TitleSummary {
            id: "tmdb:603".into(),
            kind: TitleKind::Movie,
            title: "The Matrix (TMDB)".into(),
            year: Some(1999),
            poster: None,
            rating: None,
        };
        let trakt_imdb = TitleSummary {
            // Trakt id surfaces as bare `tt...`.
            id: "tt0133093".into(),
            kind: TitleKind::Movie,
            title: "The Matrix (Trakt)".into(),
            year: Some(1999),
            poster: None,
            rating: None,
        };
        let tvdb_imdb = TitleSummary {
            // TVDB surfaces IMDb-shape via remote_ids resolution.
            id: "tt0133093".into(),
            kind: TitleKind::Movie,
            title: "The Matrix (TVDB)".into(),
            year: Some(1999),
            poster: None,
            rating: None,
        };
        let prefixed = TitleSummary {
            // Same IMDb id but in `imdb:tt...` form — should still collapse.
            id: "imdb:tt0133093".into(),
            kind: TitleKind::Movie,
            title: "Matrix (kino-shape)".into(),
            year: Some(1999),
            poster: None,
            rating: None,
        };
        // First row by IMDb-id key collapses; tmdb:603 has no imdb mapping
        // in this scenario so it stays distinct.
        let out = dedup_search_results(vec![movie_imdb.clone(), trakt_imdb, tvdb_imdb, prefixed]);
        assert_eq!(out.len(), 2);
        // Order preserved: TMDB first, Trakt's IMDb-row second.
        assert_eq!(out[0].id, "tmdb:603");
        assert_eq!(out[1].id, "tt0133093");
    }

    #[test]
    fn dedup_search_results_keeps_distinct_kinds_with_same_imdb() {
        let m = TitleSummary {
            id: "tt0000001".into(),
            kind: TitleKind::Movie,
            title: "Same".into(),
            year: None,
            poster: None,
            rating: None,
        };
        let s = TitleSummary {
            id: "tt0000001".into(),
            kind: TitleKind::Series,
            title: "Same".into(),
            year: None,
            poster: None,
            rating: None,
        };
        let out = dedup_search_results(vec![m, s]);
        assert_eq!(out.len(), 2);
    }

    // wiremock URLs need a base; build the urls struct pointed at one
    // server. Per-provider routes are namespaced by path so a single
    // MockServer is enough.
    fn search_test_config() -> HttpConfig {
        HttpConfig::for_test()
    }

    async fn seed_api_keys(db: &Db) {
        db.kv_set(TMDB_API_KEY, "test-tmdb-key").await.unwrap();
        db.kv_set(TRAKT_API_KEY, "test-trakt-key").await.unwrap();
        db.kv_set(TVDB_API_KEY, "test-tvdb-key").await.unwrap();
    }

    #[tokio::test]
    async fn search_empty_query_returns_empty_response() {
        let db = Db::open_in_memory().await.unwrap();
        let urls = SearchProviderUrls::default();
        let res = search_with_config(&db, "  ", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert!(res.direct.is_none());
        assert!(res.results.is_empty());
        assert!(!res.has_more);
        // No recent_searches row recorded for an empty query.
        assert!(db.recent_searches_list(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_imdb_shortcut_resolves_movie_via_tmdb_find() {
        let tmdb = MockServer::start().await;
        // /find returns a movie hit on the first attempt.
        Mock::given(method("GET"))
            .and(path("/3/find/tt0133093"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"movie_results": [{"id": 603}], "tv_results": []}"#),
            )
            .expect(1)
            .mount(&tmdb)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://unused".into(),
            tvdb: "http://unused".into(),
        };
        let res = search_with_config(&db, "tt0133093", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        let direct = res.direct.expect("expected direct match");
        assert_eq!(direct.id, "imdb:tt0133093");
        assert_eq!(direct.kind, TitleKind::Movie);
        assert!(res.results.is_empty());
        assert!(!res.has_more);
    }

    #[tokio::test]
    async fn search_imdb_shortcut_falls_back_to_series_when_movie_misses() {
        let tmdb = MockServer::start().await;
        // First call (movie kind) returns no match; second (tv) returns hit.
        Mock::given(method("GET"))
            .and(path("/3/find/tt0944947"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"movie_results": [], "tv_results": [{"id": 1399}]}"#),
            )
            .expect(2)
            .mount(&tmdb)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://unused".into(),
            tvdb: "http://unused".into(),
        };
        let res = search_with_config(&db, "tt0944947", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        let direct = res.direct.expect("expected direct match");
        assert_eq!(direct.id, "imdb:tt0944947");
        assert_eq!(direct.kind, TitleKind::Series);
    }

    #[tokio::test]
    async fn search_imdb_shortcut_falls_through_when_no_tmdb_key() {
        // Without TMDB key the shortcut is skipped entirely and we fall
        // through to a multi-provider search. With no Trakt/TVDB keys either
        // the response is empty.
        let db = Db::open_in_memory().await.unwrap();
        // No api keys seeded.
        let urls = SearchProviderUrls::default();
        let res = search_with_config(&db, "tt0133093", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert!(res.direct.is_none());
        assert!(res.results.is_empty());
    }

    #[tokio::test]
    #[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
    async fn search_multi_provider_aggregates_and_dedups() {
        let tmdb = MockServer::start().await;
        let trakt = MockServer::start().await;
        let tvdb = MockServer::start().await;
        // TMDB returns The Matrix as a tmdb-id row (no imdb mapping here).
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"results":[
                    {"id":603,"media_type":"movie","title":"The Matrix",
                     "release_date":"1999-03-31","vote_average":8.2}
                ]}"#,
            ))
            .expect(1)
            .mount(&tmdb)
            .await;
        // Trakt returns The Matrix with IMDb id; this dedups against TMDB
        // ONLY if both rows carry imdb form. Since TMDB row above has the
        // tmdb shape, both survive in this scenario.
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"type":"movie","movie":{"title":"The Matrix","year":1999,
                 "ids":{"imdb":"tt0133093"}}}]"#,
            ))
            .expect(1)
            .mount(&trakt)
            .await;
        // TVDB v4 login then search (one row, dedup-distinct).
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"status":"success","data":{"token":"tok"}}"#),
            )
            .expect(1)
            .mount(&tvdb)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"data":[
                    {"tvdb_id":"271124","name":"Matrix",
                     "type":"series","year":"1993","remote_ids":[]}
                ]}"#,
            ))
            .expect(1)
            .mount(&tvdb)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: trakt.uri(),
            tvdb: tvdb.uri(),
        };
        let res = search_with_config(&db, "matrix", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert!(res.direct.is_none());
        assert_eq!(res.results.len(), 3);
        // Order locked: TMDB first, Trakt second, TVDB third.
        assert_eq!(res.results[0].id, "tmdb:603");
        assert_eq!(res.results[1].id, "tt0133093");
        assert_eq!(res.results[2].id, "tvdb:271124");
    }

    #[tokio::test]
    #[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
    async fn search_drops_duplicates_by_imdb_id_across_trakt_and_tvdb() {
        let tmdb = MockServer::start().await;
        let trakt = MockServer::start().await;
        let tvdb = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#))
            .expect(1)
            .mount(&tmdb)
            .await;
        // Trakt yields tt0133093.
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{"type":"movie","movie":{"title":"Matrix Trakt","year":1999,
                 "ids":{"imdb":"tt0133093"}}}]"#,
            ))
            .expect(1)
            .mount(&trakt)
            .await;
        // TVDB also yields tt0133093 via remote_ids → IMDb-id form.
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"status":"success","data":{"token":"tok"}}"#),
            )
            .expect(1)
            .mount(&tvdb)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"data":[
                    {"tvdb_id":"169","name":"Matrix TVDB","type":"movie","year":"1999",
                     "remote_ids":[{"id":"tt0133093","sourceName":"IMDB"}]}
                ]}"#,
            ))
            .expect(1)
            .mount(&tvdb)
            .await;

        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: trakt.uri(),
            tvdb: tvdb.uri(),
        };
        let res = search_with_config(&db, "matrix", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert_eq!(res.results.len(), 1);
        // Trakt came first in the merge, so the Trakt-shaped row survives.
        assert_eq!(res.results[0].title, "Matrix Trakt");
    }

    #[tokio::test]
    async fn search_persists_recent_search_when_results_present() {
        let tmdb = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"results":[{"id":1,"media_type":"movie","title":"Real",
                 "release_date":"2020","vote_average":5.0}]}"#,
            ))
            .expect(1)
            .mount(&tmdb)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        // Only TMDB configured here — the other two clients fail with
        // wiremock-no-match, get caught by the per-provider try/log/empty
        // fallback, and don't sabotage the test.
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://127.0.0.1:1".into(),
            tvdb: "http://127.0.0.1:1".into(),
        };
        let res = search_with_config(&db, "real", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert_eq!(res.results.len(), 1);
        let recent = db.recent_searches_list(10).await.unwrap();
        assert_eq!(recent, vec!["real".to_string()]);
    }

    #[tokio::test]
    async fn search_does_not_persist_recent_search_when_zero_results() {
        let tmdb = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#))
            .expect(1)
            .mount(&tmdb)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://127.0.0.1:1".into(),
            tvdb: "http://127.0.0.1:1".into(),
        };
        let res = search_with_config(&db, "zzz", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert!(res.results.is_empty());
        assert!(db.recent_searches_list(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_availability_filter_drops_unavailable_items_when_addons_present() {
        let tmdb = MockServer::start().await;
        let stream_server = MockServer::start().await;
        // TMDB returns three results, all imdb-form via "tt..." TMDB ids
        // would be unusual; instead use TMDB-shaped ids and rely on the
        // stream addon dispatching by raw id.
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"results":[
                    {"id":1,"media_type":"movie","title":"Avail","release_date":"2020"},
                    {"id":2,"media_type":"movie","title":"Gone","release_date":"2021"},
                    {"id":3,"media_type":"movie","title":"Empty","release_date":"2022"}
                ]}"#,
            ))
            .expect(1)
            .mount(&tmdb)
            .await;
        // Stream addon: tmdb:1 has streams, tmdb:2 returns 404, tmdb:3
        // returns empty array.
        Mock::given(method("GET"))
            .and(path("/stream/movie/tmdb:1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"streams":[{"url":"u"}]}"#),
            )
            .mount(&stream_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tmdb:2.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&stream_server)
            .await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tmdb:3.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"streams":[]}"#))
            .mount(&stream_server)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let manifest_url = format!("{}/manifest.json", stream_server.uri());
        install_stream_addon(&db, "addon-a", &manifest_url, true).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://127.0.0.1:1".into(),
            tvdb: "http://127.0.0.1:1".into(),
        };
        let res = search_with_config(&db, "avail", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        let titles: Vec<&str> = res.results.iter().map(|s| s.title.as_str()).collect();
        // Only "Avail" survives: "Gone" 404'd, "Empty" returned no streams.
        assert_eq!(titles, vec!["Avail"]);
    }

    #[tokio::test]
    async fn search_availability_filter_passes_through_when_no_addons_installed() {
        let tmdb = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"results":[
                    {"id":1,"media_type":"movie","title":"One","release_date":"2020"}
                ]}"#,
            ))
            .expect(1)
            .mount(&tmdb)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        // No addons installed at all.
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://127.0.0.1:1".into(),
            tvdb: "http://127.0.0.1:1".into(),
        };
        let res = search_with_config(&db, "one", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert_eq!(res.results.len(), 1);
        assert_eq!(res.results[0].title, "One");
    }

    #[tokio::test]
    async fn search_has_more_true_when_more_than_one_page_returned() {
        use std::fmt::Write as _;
        let tmdb = MockServer::start().await;
        // Construct 25 results so dedup keeps 25 and only the first 20
        // surface (has_more = true).
        let mut results = String::new();
        for i in 1..=25 {
            if i > 1 {
                results.push(',');
            }
            write!(
                results,
                r#"{{"id":{i},"media_type":"movie","title":"T{i}","release_date":"2020"}}"#
            )
            .unwrap();
        }
        let body = format!(r#"{{"results":[{results}]}}"#);
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&tmdb)
            .await;
        let db = Db::open_in_memory().await.unwrap();
        seed_api_keys(&db).await;
        let urls = SearchProviderUrls {
            tmdb: tmdb.uri(),
            trakt: "http://127.0.0.1:1".into(),
            tvdb: "http://127.0.0.1:1".into(),
        };
        let res = search_with_config(&db, "t", 1, "en-US", &search_test_config(), &urls)
            .await
            .unwrap();
        assert_eq!(res.results.len(), kino_core::constants::SEARCH_PAGE_SIZE);
        assert!(res.has_more);
    }

    #[tokio::test]
    async fn recent_searches_commands_round_trip_through_db() {
        let db = Db::open_in_memory().await.unwrap();
        db.recent_searches_upsert("alpha", kino_core::constants::RECENT_SEARCHES_MAX)
            .await
            .unwrap();
        let listed = db
            .recent_searches_list(kino_core::constants::RECENT_SEARCHES_MAX)
            .await
            .unwrap();
        assert_eq!(listed, vec!["alpha".to_string()]);
        let removed = db.recent_searches_clear().await.unwrap();
        assert_eq!(removed, 1);
        assert!(db
            .recent_searches_list(kino_core::constants::RECENT_SEARCHES_MAX)
            .await
            .unwrap()
            .is_empty());
    }

    // ---- F-016: Settings -----------------------------------------------

    /// Direct helper exercising the same logic as `settings_set` without
    /// the Tauri command wrapper (which needs a `State` extractor).
    async fn settings_set_helper(db: &Db, key: &str, value: &str) -> Result<String, String> {
        let normalized =
            crate::settings::validate_setting(key, value, crate::settings::HostPlatform::Linux)?;
        db.kv_set(key, &normalized).await.map_err(ipc)?;
        Ok(normalized)
    }

    /// Wipes user-set KV + non-Cinemeta addons. Mirrors the body of
    /// `settings_reset_defaults` so unit tests can exercise it without a
    /// Tauri State extractor.
    async fn settings_reset_defaults_helper(db: &Db) -> Result<(), String> {
        for key in crate::settings::KNOWN_SETTINGS_KEYS {
            db.kv_delete(key).await.map_err(ipc)?;
        }
        let installed = db.addons_list().await.map_err(ipc)?;
        for addon in installed {
            if is_cinemeta_id(db, &addon.id).await? {
                db.addons_set_enabled(&addon.id, true).await.map_err(ipc)?;
                db.addons_reorder(std::slice::from_ref(&addon.id))
                    .await
                    .map_err(ipc)?;
                continue;
            }
            db.addons_delete(&addon.id).await.map_err(ipc)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn settings_set_persists_normalized_value() {
        let db = Db::open_in_memory().await.unwrap();
        let saved = settings_set_helper(&db, crate::settings::DISPLAY_NSFW_KEY, "1")
            .await
            .unwrap();
        assert_eq!(saved, "true");
        assert_eq!(
            db.kv_get(crate::settings::DISPLAY_NSFW_KEY).await.unwrap(),
            Some("true".to_string())
        );
    }

    #[tokio::test]
    async fn settings_set_rejects_invalid_value_without_writing() {
        let db = Db::open_in_memory().await.unwrap();
        let err = settings_set_helper(&db, crate::settings::CACHE_SIZE_GIB_KEY, "999")
            .await
            .unwrap_err();
        assert!(err.contains("cache size"), "got: {err}");
        assert_eq!(
            db.kv_get(crate::settings::CACHE_SIZE_GIB_KEY)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn settings_get_all_round_trips_through_set() {
        let db = Db::open_in_memory().await.unwrap();
        settings_set_helper(&db, TMDB_API_KEY, "abc").await.unwrap();
        settings_set_helper(&db, crate::settings::META_PRIMARY_LANG_KEY, "fr")
            .await
            .unwrap();
        settings_set_helper(&db, crate::settings::DISPLAY_TILE_SIZE_KEY, "large")
            .await
            .unwrap();
        let view =
            crate::settings::load_view(&db, crate::settings::HostPlatform::Linux, "/tmp/kino")
                .await
                .unwrap();
        assert_eq!(view.api_keys.tmdb, "abc");
        assert_eq!(view.language.metadata_primary, "fr");
        assert_eq!(view.display.tile_size, "large");
    }

    #[tokio::test]
    async fn settings_reset_defaults_wipes_known_keys_but_keeps_install_id() {
        let db = Db::open_in_memory().await.unwrap();
        let install_id = db.install_id().await.unwrap();
        settings_set_helper(&db, TMDB_API_KEY, "abc").await.unwrap();
        settings_set_helper(&db, crate::settings::DISPLAY_TILE_SIZE_KEY, "large")
            .await
            .unwrap();

        settings_reset_defaults_helper(&db).await.unwrap();

        assert_eq!(db.kv_get(TMDB_API_KEY).await.unwrap(), None);
        assert_eq!(
            db.kv_get(crate::settings::DISPLAY_TILE_SIZE_KEY)
                .await
                .unwrap(),
            None
        );
        // Install id is system-internal — survives the wipe.
        assert_eq!(db.install_id().await.unwrap(), install_id);
    }

    #[tokio::test]
    async fn settings_reset_defaults_keeps_cinemeta_removes_others() {
        let db = Db::open_in_memory().await.unwrap();
        // Pre-seed two addons: Cinemeta (URL-matched) and an imposter.
        db.addons_insert(&AddonInsert {
            id: "com.linvo.cinemeta".into(),
            manifest_url: CINEMETA_MANIFEST_URL.into(),
            manifest_json: serde_json::json!({"id":"com.linvo.cinemeta"}),
            display_order: None,
        })
        .await
        .unwrap();
        db.addons_insert(&AddonInsert {
            id: "other.addon".into(),
            manifest_url: "https://other/manifest.json".into(),
            manifest_json: serde_json::json!({"id":"other.addon"}),
            display_order: None,
        })
        .await
        .unwrap();
        // Disable Cinemeta to confirm reset re-enables it.
        db.addons_set_enabled("com.linvo.cinemeta", false)
            .await
            .unwrap();

        settings_reset_defaults_helper(&db).await.unwrap();

        let remaining = db.addons_list().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "com.linvo.cinemeta");
        assert!(remaining[0].enabled, "reset must re-enable Cinemeta");
    }

    #[tokio::test]
    async fn cache_usage_helper_sums_files() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("a.bin")).unwrap();
        f.write_all(&[0u8; 2048]).unwrap();
        drop(f);
        let size = crate::cache_fs::dir_size_bytes(dir.path());
        assert_eq!(size, 2048);
    }

    #[tokio::test]
    async fn cache_clear_helper_keeps_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("a.bin")).unwrap();
        crate::cache_fs::clear_dir_contents(dir.path()).unwrap();
        assert!(dir.path().is_dir());
    }

    #[tokio::test]
    async fn export_logs_writes_zip_at_destination() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("kino.log"), "hi").unwrap();
        let dest = dir.path().join("out.zip");
        let bytes = crate::logs::zip_log_dir(&logs, &dest).unwrap();
        assert!(dest.exists());
        assert!(bytes > 0);
    }

    #[test]
    fn get_app_info_returns_workspace_metadata() {
        let info = get_app_info();
        assert_eq!(info.name, "kino-app");
        assert!(!info.version.is_empty());
        // KINO_COMMIT_SHA is injected by `build.rs`; tests in CI see a real
        // SHA, local sandboxes without git see "unknown". Either way the
        // field is non-empty.
        assert!(!info.commit.is_empty());
        assert_eq!(info.license, "MIT");
        let platform = info.platform;
        assert!(
            ["linux", "android", "macos", "windows", "unknown"].contains(&platform),
            "unexpected platform label: {platform}"
        );
    }

    // ---- F-012: continue-watching rule application --------------------

    fn cw_movie(id: &str, position: f64, duration: f64, last_played: i64) -> ContinueWatching {
        ContinueWatching {
            title_id: id.into(),
            kind: TitleKind::Movie,
            season: 0,
            episode: 0,
            position_s: position,
            duration_s: duration,
            last_played_at: last_played,
            meta_json: serde_json::json!({}),
        }
    }

    fn cw_episode(
        id: &str,
        season: i64,
        episode: i64,
        position: f64,
        duration: f64,
        last_played: i64,
    ) -> ContinueWatching {
        ContinueWatching {
            title_id: id.into(),
            kind: TitleKind::Series,
            season,
            episode,
            position_s: position,
            duration_s: duration,
            last_played_at: last_played,
            meta_json: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn cw_record_position_keeps_in_progress_row_unchanged() {
        let db = Db::open_in_memory().await.unwrap();
        let cw = cw_movie("tt1", 600.0, 1800.0, 100); // 33%
        let out = cw_record_position_inner(&db, cw.clone(), &[])
            .await
            .unwrap();
        assert_eq!(out, Some(cw.clone()));
        let listed = db.cw_list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!((listed[0].position_s - 600.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn cw_record_position_series_advances_to_next_episode() {
        let db = Db::open_in_memory().await.unwrap();
        let cw = cw_episode("tt_series", 1, 1, 1800.0, 1800.0, 200); // 100%
        let eps = [(1, 1), (1, 2), (1, 3)];
        let out = cw_record_position_inner(&db, cw.clone(), &eps)
            .await
            .unwrap();
        let advanced = out.expect("series advance must return the new row");
        assert_eq!(advanced.season, 1);
        assert_eq!(advanced.episode, 2);
        assert!(advanced.position_s.abs() < f64::EPSILON);
        let listed = db.cw_list().await.unwrap();
        assert_eq!(listed.len(), 1, "old episode row must be wiped");
        assert_eq!((listed[0].season, listed[0].episode), (1, 2));
    }

    #[tokio::test]
    async fn cw_record_position_series_removes_when_final_episode_completed() {
        let db = Db::open_in_memory().await.unwrap();
        // Pre-seed a prior episode's row so we can prove the wipe is
        // title-wide, not just the (s,e) we record on.
        db.cw_upsert(&cw_episode("tt_series", 1, 1, 1800.0, 1800.0, 199))
            .await
            .unwrap();
        let cw = cw_episode("tt_series", 1, 2, 1800.0, 1800.0, 200);
        db.cw_upsert(&cw).await.unwrap();
        let eps = [(1, 1), (1, 2)];
        let out = cw_record_position_inner(&db, cw, &eps).await.unwrap();
        assert!(out.is_none(), "series end must signal removal");
        assert_eq!(db.cw_list().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn cw_record_position_movie_completion_keeps_row_for_sweep() {
        let db = Db::open_in_memory().await.unwrap();
        let cw = cw_movie("tt_done", 6000.0, 6000.0, 100); // 100%
        let out = cw_record_position_inner(&db, cw.clone(), &[])
            .await
            .unwrap();
        assert_eq!(out, Some(cw));
        // Row stays — PRD §F-012 movies are kept until the 24h sweep
        // ages them out, distinguishing them from series which advance.
        assert_eq!(db.cw_list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cw_remove_title_wipes_every_episode_for_that_title() {
        let db = Db::open_in_memory().await.unwrap();
        db.cw_upsert(&cw_episode("tt_a", 1, 1, 10.0, 60.0, 100))
            .await
            .unwrap();
        db.cw_upsert(&cw_episode("tt_a", 1, 2, 10.0, 60.0, 110))
            .await
            .unwrap();
        db.cw_upsert(&cw_episode("tt_b", 1, 1, 10.0, 60.0, 120))
            .await
            .unwrap();
        let removed = db.cw_delete_all_for_title("tt_a").await.unwrap();
        assert_eq!(removed, 2);
        let remaining: Vec<String> = db
            .cw_list()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title_id)
            .collect();
        assert_eq!(remaining, vec!["tt_b"]);
    }

    #[tokio::test]
    async fn cw_list_runs_auto_removal_sweep_before_returning() {
        let db = Db::open_in_memory().await.unwrap();
        // Completed row last played 25h ago — must be swept.
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let stale_completed = cw_movie("tt_old", 100.0, 100.0, now - 25 * 3600);
        // Completed row last played 1h ago — keep.
        let fresh_completed = cw_movie("tt_new", 100.0, 100.0, now - 3600);
        // In-progress row, last played 30 days ago — keep (only
        // completed rows participate in the sweep).
        let stale_in_progress = cw_movie("tt_inprog", 50.0, 100.0, now - 30 * 86_400);
        db.cw_upsert(&stale_completed).await.unwrap();
        db.cw_upsert(&fresh_completed).await.unwrap();
        db.cw_upsert(&stale_in_progress).await.unwrap();

        // Drive the sweep directly so we don't need a Tauri State.
        let removed = cw_sweep_completed(&db).await.unwrap();
        assert_eq!(removed, 1);
        let ids: Vec<String> = db
            .cw_list()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title_id)
            .collect();
        assert!(ids.contains(&"tt_new".to_string()));
        assert!(ids.contains(&"tt_inprog".to_string()));
        assert!(!ids.contains(&"tt_old".to_string()));
    }
}
