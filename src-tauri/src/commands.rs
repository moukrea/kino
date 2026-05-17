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
use kino_core::addon::{Addon, AddonInsert};
use kino_core::constants::ARTWORK_TTL_S;
use kino_core::cw::ContinueWatching;
use kino_core::title::{TitleKind, TitleSummary};
use kino_core::Db;
use kino_metadata::{
    aggregate, fetch_and_resolve, lang_chain_hash, Artwork, FanartClient, ProviderItem, TitleIds,
    TmdbClient, TraktClient, TvdbClient, FANART_API_KEY, TMDB_API_KEY, TRAKT_API_KEY, TVDB_API_KEY,
};
use tauri::State;

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

// ---- F-005: image & logo resolution -------------------------------------
//
// PRD §F-005 locks the six-tier fallback chain. The aggregator itself lives
// in `kino-metadata::artwork`; the host command stitches it onto the catalog
// id parser (so a `tmdb:603` id can drive a Fanart.tv lookup once external
// ids are resolved) and onto `response_cache` for the 7-day TTL.

/// `resolve_artwork(title_id, kind, lang_pref)` (PRD §F-005).
///
/// `title_id` accepts the catalog id forms the F-004 trending aggregator
/// emits — `imdb:tt...` / `tmdb:N` / `tvdb:N` / bare `tt...`. `kind` is
/// `Movie` or `Series`. `lang_pref` is the user's primary language plus up
/// to three fallbacks; an empty slice resolves only via the tier-5 sweep
/// then placeholders.
#[tauri::command]
#[allow(clippy::similar_names)] // PRD-locked provider names (tmdb / tvdb).
pub async fn resolve_artwork(
    db: State<'_, Db>,
    title_id: String,
    kind: TitleKind,
    lang_pref: Vec<String>,
) -> Result<Artwork, String> {
    let lang_hash = lang_chain_hash(&lang_pref);
    let cache_key = format!("artwork:{}:{title_id}:{lang_hash}", kind.as_str());

    if let Some(cached) = db.cache_get(&cache_key).await.map_err(ipc)? {
        if let Ok(parsed) = serde_json::from_str::<Artwork>(&cached) {
            return Ok(parsed);
        }
        tracing::warn!(key = %cache_key, "discarding malformed cached artwork payload");
    }

    let tmdb_key = db.kv_get(TMDB_API_KEY).await.map_err(ipc)?;
    let tvdb_key = db.kv_get(TVDB_API_KEY).await.map_err(ipc)?;
    let fanart_key = db.kv_get(FANART_API_KEY).await.map_err(ipc)?;

    let tmdb = build_optional_client(tmdb_key.as_deref(), TmdbClient::new)?;
    let tvdb = build_optional_client(tvdb_key.as_deref(), TvdbClient::new)?;
    let fanart = build_optional_client(fanart_key.as_deref(), FanartClient::new)?;

    let mut ids = parse_title_id(&title_id);
    // Best-effort cross-id enrichment via TMDB. Failures don't block the
    // resolve — the chain will simply skip the providers we lack ids for
    // and fall through to placeholders.
    if let Some(t) = tmdb.as_ref() {
        enrich_ids(&mut ids, kind, t).await;
    }

    let artwork = fetch_and_resolve(
        &ids,
        kind,
        &lang_pref,
        fanart.as_ref(),
        tmdb.as_ref(),
        tvdb.as_ref(),
    )
    .await;

    let payload = serde_json::to_string(&artwork).map_err(|e| e.to_string())?;
    let expires_at = i64::try_from(
        u64::try_from(now_unix_seconds())
            .unwrap_or(0)
            .saturating_add(ARTWORK_TTL_S),
    )
    .unwrap_or(i64::MAX);
    if let Err(e) = db.cache_set(&cache_key, &payload, expires_at).await {
        tracing::warn!(error = %e, key = %cache_key, "failed to persist artwork cache");
    }

    Ok(artwork)
}

fn build_optional_client<T, F>(key: Option<&str>, ctor: F) -> Result<Option<T>, String>
where
    F: FnOnce(String) -> Result<T, kino_metadata::Error>,
{
    match key {
        Some(k) if !k.is_empty() => Ok(Some(ctor(k.to_string()).map_err(ipc)?)),
        _ => Ok(None),
    }
}

/// Parse a catalog id of the form `imdb:tt...` / `tmdb:N` / `tvdb:N` or a
/// bare `tt...` `IMDb` id into a [`TitleIds`] bag. Unknown forms (e.g.
/// `trakt-rank:N`) yield an empty bag; the resolver will fall through to
/// placeholders.
fn parse_title_id(raw: &str) -> TitleIds {
    let mut ids = TitleIds::default();
    if let Some(rest) = raw.strip_prefix("imdb:") {
        ids.imdb = Some(rest.to_string());
        return ids;
    }
    if let Some(rest) = raw.strip_prefix("tmdb:") {
        if let Ok(n) = rest.parse::<u64>() {
            ids.tmdb = Some(n);
        }
        return ids;
    }
    if let Some(rest) = raw.strip_prefix("tvdb:") {
        if let Ok(n) = rest.parse::<u64>() {
            ids.tvdb = Some(n);
        }
        return ids;
    }
    if raw.starts_with("tt") && raw.len() > 2 {
        ids.imdb = Some(raw.to_string());
    }
    ids
}

/// Fill in the IDs the parsed catalog id did not carry, using TMDB
/// `/find` (IMDb→TMDB) and `/external_ids` (TMDB→IMDb/TVDB). Best-effort:
/// each failure is logged at warn level and the field stays `None`.
async fn enrich_ids(ids: &mut TitleIds, kind: TitleKind, tmdb: &TmdbClient) {
    // IMDb known but TMDB missing → /find.
    if ids.tmdb.is_none() {
        if let Some(imdb) = ids.imdb.clone() {
            match tmdb.find_by_external_id(kind, "imdb_id", &imdb).await {
                Ok(Some(n)) => ids.tmdb = Some(n),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, imdb, "tmdb /find failed"),
            }
        } else if let Some(tvdb_id) = ids.tvdb {
            match tmdb
                .find_by_external_id(kind, "tvdb_id", &tvdb_id.to_string())
                .await
            {
                Ok(Some(n)) => ids.tmdb = Some(n),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, tvdb_id, "tmdb /find failed"),
            }
        }
    }
    // TMDB known → /external_ids to fill IMDb / TVDB.
    if let Some(tmdb_id) = ids.tmdb {
        if ids.imdb.is_none() || ids.tvdb.is_none() {
            match tmdb.fetch_external_ids(kind, tmdb_id).await {
                Ok(external) => {
                    if ids.imdb.is_none() {
                        ids.imdb = external.imdb;
                    }
                    if ids.tvdb.is_none() {
                        ids.tvdb = external.tvdb;
                    }
                }
                Err(e) => tracing::warn!(error = %e, tmdb_id, "tmdb /external_ids failed"),
            }
        }
    }
}

fn now_unix_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
    fn parse_title_id_handles_each_prefix() {
        let ids = parse_title_id("imdb:tt0133093");
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tmdb, None);
        assert_eq!(ids.tvdb, None);

        let ids = parse_title_id("tmdb:603");
        assert_eq!(ids.tmdb, Some(603));

        let ids = parse_title_id("tvdb:78878");
        assert_eq!(ids.tvdb, Some(78_878));

        // Bare IMDb id.
        let ids = parse_title_id("tt0944947");
        assert_eq!(ids.imdb.as_deref(), Some("tt0944947"));
    }

    #[test]
    fn parse_title_id_rejects_unknown_prefix() {
        let ids = parse_title_id("trakt-rank:7");
        assert_eq!(ids.imdb, None);
        assert_eq!(ids.tmdb, None);
        assert_eq!(ids.tvdb, None);

        // `tt` prefix without numbers stays unparsed too.
        let ids = parse_title_id("tt");
        assert_eq!(ids.imdb, None);
    }

    #[test]
    fn parse_title_id_rejects_non_numeric_tmdb() {
        let ids = parse_title_id("tmdb:abc");
        assert_eq!(ids.tmdb, None);
    }
}
