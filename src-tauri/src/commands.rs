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
use kino_core::title::{Artwork, TitleKind, TitleSummary};
use kino_core::Db;
use kino_metadata::artwork::{cascade, lang_chain_hash, CachedArtwork, ProviderBundles};
use kino_metadata::tmdb::TitleIds;
use kino_metadata::{
    aggregate, FanartClient, ProviderItem, TmdbClient, TraktClient, TvdbClient, FANART_API_KEY,
    TMDB_API_KEY, TRAKT_API_KEY, TVDB_API_KEY,
};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
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
}
