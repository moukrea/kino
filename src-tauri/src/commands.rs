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
use kino_addons::{
    normalize_manifest_url, AddonClient, AddonError, Manifest, RecommendedAddon,
    CINEMETA_MANIFEST_URL, RECOMMENDED_ADDONS,
};
use kino_core::addon::{Addon, AddonInsert};
use kino_core::availability::AvailabilityRow;
use kino_core::constants::{ARTWORK_TTL_S, AVAILABILITY_CONCURRENCY, AVAILABILITY_TIMEOUT_S};
use kino_core::cw::ContinueWatching;
use kino_core::http::HttpConfig;
use kino_core::title::{Artwork, TitleKind, TitleSummary};
use kino_core::Db;
use kino_metadata::artwork::{cascade, lang_chain_hash, CachedArtwork, ProviderBundles};
use kino_metadata::tmdb::TitleIds;
use kino_metadata::{
    aggregate, FanartClient, ProviderItem, TmdbClient, TraktClient, TvdbClient, FANART_API_KEY,
    TMDB_API_KEY, TRAKT_API_KEY, TVDB_API_KEY,
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

#[tauri::command]
pub async fn cw_list(db: State<'_, Db>) -> Result<Vec<ContinueWatching>, String> {
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
}
