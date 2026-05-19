//! TMDB v3 client (PRD §F-003, §F-004).
//!
//! TMDB serves trending, search, find, movie, tv, and configuration. This
//! module exposes the credential-test endpoint (F-003) and the weekly
//! trending catalogs consumed by the F-004 aggregator. Detail / search land
//! with F-010 and F-011.
//!
//! Authentication: the v3 `api_key` is passed as a query parameter on every
//! request. Stored in `settings.tmdb_api_key` (see [`crate::TMDB_API_KEY`]).

use serde::{Deserialize, Serialize};

use crate::artwork::{LocalizedAsset, ProviderBundle};
use crate::error::Error;
use crate::trending::ProviderItem;
use kino_core::http::{fetch_with_etag, fetch_with_retry, FetchOutcome, HttpConfig};
use kino_core::title::{TitleKind, TitleSummary};

/// Production TMDB base URL.
pub const TMDB_BASE_URL: &str = "https://api.themoviedb.org";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

/// External-id resolution result. Each field is `None` when TMDB has no
/// corresponding mapping; the F-005 resolver uses these to dispatch the
/// Fanart.tv (needs `IMDb` or TMDB id for movies, TVDB id for series) and
/// TVDB (needs TVDB id) calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleIds {
    pub tmdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u64>,
}

/// Detail attributes for the PRD §F-010 title-detail view. Sourced from
/// TMDB `/3/{movie,tv}/{id}` with `append_to_response=release_dates`
/// (movie) / `content_ratings` (tv).
///
/// `Serialize` / `Deserialize` are derived so the value can be persisted
/// in `response_cache` for the per-resource `ETag` round-trip (PRD §F-003;
/// Session 031).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TmdbTitleDetails {
    pub tmdb_id: u64,
    pub kind: TitleKind,
    /// BCP-47-ish language the overview is translated into.
    pub language: String,
    /// Movie: `runtime` (minutes). Series: first non-zero entry from
    /// `episode_run_time[]`. `None` when neither is available.
    pub runtime_minutes: Option<u32>,
    /// US certification (movies: `release_dates.results[iso_3166_1=US]` →
    /// `release_dates[0].certification`; series:
    /// `content_ratings.results[iso_3166_1=US].rating`). `None` when not
    /// known.
    pub age_rating: Option<String>,
    /// Genre names in TMDB's locale-translated form.
    pub genres: Vec<String>,
    /// Localized overview text; `None` when empty / missing.
    pub overview: Option<String>,
    /// TMDB user rating (`vote_average` on the 0-10 scale). `None` when
    /// TMDB has no votes.
    pub rating: Option<f64>,
}

/// Result of [`TmdbClient::title_details_with_etag`] — either a fresh
/// payload (with the server's `ETag` header, when present) or the cache-hit
/// `304 Not Modified` signal that the caller's prior cached
/// [`TmdbTitleDetails`] is still current. PRD §F-003 round-trip.
#[derive(Debug, Clone, PartialEq)]
pub enum TmdbTitleDetailsFetch {
    /// Server confirmed the caller's cached payload is still current. The
    /// caller re-uses its existing [`TmdbTitleDetails`] and only refreshes
    /// the cache row's `expires_at` (see
    /// [`kino_core::Db::cache_refresh_expiry`]).
    NotModified,
    /// Server returned a fresh body. `etag` is the parsed `ETag` header
    /// (or `None` if the endpoint didn't send one); persist it alongside
    /// the new payload in `response_cache.etag`.
    Fresh {
        details: TmdbTitleDetails,
        etag: Option<String>,
    },
}

/// One cast member, top-level for the PRD §F-010 cast row.
///
/// `Serialize` / `Deserialize` are derived so the value can be persisted in
/// `response_cache` for the per-resource `ETag` round-trip (PRD §F-003;
/// Session 032).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmdbCastMember {
    pub name: String,
    pub character: Option<String>,
    pub photo_url: Option<String>,
}

/// Cast roster wrapper persisted in `response_cache` for the per-resource
/// `ETag` round-trip on TMDB `/credits` (PRD §F-003; Session 032).
///
/// Wrapping the `Vec<TmdbCastMember>` in a named struct keeps the JSON
/// payload self-describing on cache reads and mirrors the
/// [`TmdbTitleDetails`] shape (one resource, one struct).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmdbCredits {
    pub tmdb_id: u64,
    pub kind: TitleKind,
    pub cast: Vec<TmdbCastMember>,
}

/// Result of [`TmdbClient::credits_with_etag`] — either a fresh roster
/// (with the server's `ETag` header, when present) or the cache-hit `304
/// Not Modified` signal that the caller's prior cached [`TmdbCredits`] is
/// still current. PRD §F-003 round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmdbCreditsFetch {
    /// Server confirmed the caller's cached cast list is still current.
    /// The caller re-uses its existing [`TmdbCredits`] and only refreshes
    /// the cache row's `expires_at`.
    NotModified,
    /// Server returned a fresh body. `etag` is the parsed `ETag` header
    /// (or `None` if the endpoint didn't send one); persist it alongside
    /// the new payload in `response_cache.etag`.
    Fresh {
        credits: TmdbCredits,
        etag: Option<String>,
    },
}

/// TMDB v3 API client.
#[derive(Debug, Clone)]
pub struct TmdbClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl TmdbClient {
    /// Build a client pointed at the production TMDB v3 endpoint.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(key, HttpConfig::default(), TMDB_BASE_URL.to_string())
    }

    /// Build a client with an explicit `HttpConfig` and base URL. Used by
    /// tests (wiremock) and by future cache wiring (F-004+) that needs to
    /// inject a longer timeout.
    pub fn with_options(
        key: impl Into<String>,
        config: HttpConfig,
        base_url: String,
    ) -> Result<Self, Error> {
        let client = config.build_client()?;
        Ok(Self {
            key: key.into(),
            base_url,
            config,
            client,
        })
    }

    /// Verify the stored API key against `/3/configuration`. PRD §F-003
    /// requires this for every provider.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        let url = format!("{}/3/configuration", self.base_url);
        fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        Ok(())
    }

    /// Fetch the trending movies for the past week (PRD §F-004 step 1).
    ///
    /// `locale` is the BCP-47 language tag forwarded to TMDB's `language`
    /// query parameter so titles come back localized when possible. TMDB's
    /// `language` only affects the `title` / `overview` fields; the ranking
    /// itself is global. Items come back already ranked by trending position
    /// (best first); the aggregator assigns `rank = 0` to the first.
    pub async fn trending_movies(&self, locale: &str) -> Result<Vec<ProviderItem>, Error> {
        self.fetch_trending("movie", locale, TitleKind::Movie).await
    }

    /// Fetch the trending TV shows for the past week (PRD §F-004 step 1).
    /// See [`trending_movies`](Self::trending_movies) for the locale rules.
    pub async fn trending_shows(&self, locale: &str) -> Result<Vec<ProviderItem>, Error> {
        self.fetch_trending("tv", locale, TitleKind::Series).await
    }

    /// Resolve a TMDB id from an external id (`IMDb` or TVDB) via `/3/find`.
    /// Returns the first matching numeric TMDB id from the appropriate
    /// `*_results` array, or `None` when TMDB has no mapping.
    ///
    /// `external_source` must be one of `"imdb_id"` or `"tvdb_id"` per TMDB
    /// documentation. `kind` selects which `*_results` array to read
    /// (`movie_results` vs `tv_results`).
    pub async fn find_external(
        &self,
        external_id: &str,
        external_source: &'static str,
        kind: TitleKind,
    ) -> Result<Option<u64>, Error> {
        let url = format!("{}/3/find/{external_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("external_source", external_source),
                ])
            },
            &self.config,
        )
        .await?;
        let body: FindResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb find {external_source}: {e}")))?;
        let results = match kind {
            TitleKind::Movie => body.movie_results,
            TitleKind::Series => body.tv_results,
        };
        Ok(results.into_iter().next().map(|r| r.id))
    }

    /// Fetch external ids (`imdb_id`, `tvdb_id`) for a TMDB title. Used by
    /// the F-005 resolver to bridge into Fanart.tv (movies key by `IMDb`,
    /// shows key by TVDB) and TVDB (keys by its own id).
    pub async fn external_ids(&self, tmdb_id: u64, kind: TitleKind) -> Result<TitleIds, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}/external_ids", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        let body: ExternalIdsResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb external_ids: {e}")))?;
        Ok(TitleIds {
            tmdb_id: Some(tmdb_id),
            imdb_id: body.imdb_id.filter(|s| !s.is_empty()),
            tvdb_id: body.tvdb_id,
        })
    }

    /// Fetch the F-005 image bundle for a title. One HTTP call covers every
    /// language we care about by joining `lang_pref` (plus `null` for
    /// textless artwork) into TMDB's `include_image_language` filter. The
    /// response carries `posters` / `backdrops` / `logos`; TMDB does not
    /// expose clearart so [`ProviderBundle::clearart`] stays empty.
    pub async fn artwork_images(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        lang_pref: &[String],
    ) -> Result<ProviderBundle, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}/images", self.base_url);
        let langs = include_image_language(lang_pref);
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("include_image_language", langs.as_str()),
                ])
            },
            &self.config,
        )
        .await?;
        let body: ImagesResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb images {media}: {e}")))?;
        Ok(ProviderBundle {
            posters: body.posters.into_iter().map(into_asset).collect(),
            backdrops: body.backdrops.into_iter().map(into_asset).collect(),
            logos: body.logos.into_iter().map(into_asset).collect(),
            clearart: Vec::new(),
            summaries: std::collections::HashMap::new(),
        })
    }

    /// Fetch the localized summary for a title in `language`. Returns
    /// `Some(text)` when the overview is non-empty, `None` otherwise. The
    /// F-005 resolver may call this several times per title (one per tier
    /// language) so the host caches at the cascade granularity, not here.
    pub async fn summary(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        language: &str,
    ) -> Result<Option<String>, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str()), ("language", language)])
            },
            &self.config,
        )
        .await?;
        let body: DetailResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media} details: {e}")))?;
        Ok(body.overview.filter(|s| !s.is_empty()))
    }

    /// Fetch detail attributes for the title-detail view (PRD §F-010):
    /// runtime in minutes, age rating (US `release_dates` for movies, US
    /// `content_ratings` for TV), genres, vote average, and the localized
    /// overview. One round-trip per language; the caller does its own
    /// caching at the `TitleDetail` granularity (`META_TTL_S = 24h`).
    ///
    /// `release_dates` / `content_ratings` are pulled in the same call via
    /// `append_to_response`. Empty results yield `None` rather than an
    /// empty string so the UI can elide missing fields cleanly.
    pub async fn title_details(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        language: &str,
    ) -> Result<TmdbTitleDetails, Error> {
        match self
            .title_details_with_etag(tmdb_id, kind, language, None)
            .await?
        {
            TmdbTitleDetailsFetch::Fresh { details, .. } => Ok(details),
            // Unreachable: `prior_etag = None` means the server cannot
            // legally produce a 304. Treat as decode error in case a
            // misbehaving mock yields one.
            TmdbTitleDetailsFetch::NotModified => Err(Error::Decode(
                "tmdb title_details: 304 returned without prior etag".to_string(),
            )),
        }
    }

    /// `ETag`-aware variant of [`title_details`](Self::title_details) for
    /// cache revalidation (PRD §F-003). Pass the cache row's stored `ETag`
    /// as `prior_etag`; on `304 Not Modified` the server confirms the cached
    /// payload is still current and the caller re-uses it (refreshing the
    /// row's `expires_at`). On a fresh `2xx`, the parsed `ETag` header is
    /// returned alongside the new payload for persistence.
    pub async fn title_details_with_etag(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        language: &str,
        prior_etag: Option<&str>,
    ) -> Result<TmdbTitleDetailsFetch, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let append = match kind {
            TitleKind::Movie => "release_dates",
            TitleKind::Series => "content_ratings",
        };
        let url = format!("{}/3/{media}/{tmdb_id}", self.base_url);
        let outcome = fetch_with_etag(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("language", language),
                    ("append_to_response", append),
                ])
            },
            prior_etag,
            &self.config,
        )
        .await?;
        let (response, etag) = match outcome {
            FetchOutcome::NotModified => return Ok(TmdbTitleDetailsFetch::NotModified),
            FetchOutcome::Fresh { response, etag } => (response, etag),
        };
        let body: TitleDetailResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media} title_details: {e}")))?;
        let details = parse_title_details(body, tmdb_id, kind, language);
        Ok(TmdbTitleDetailsFetch::Fresh { details, etag })
    }

    /// Fetch the cast credits for a title (PRD §F-010 cast row).
    ///
    /// Returns the cast roster in TMDB's own ordering (already ranked by
    /// billing); the caller (`get_title_detail`) truncates to the top six
    /// for display. Missing cast (e.g. a Stremio-only show TMDB doesn't
    /// know about) yields an empty list, not an error.
    pub async fn credits(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
    ) -> Result<Vec<TmdbCastMember>, Error> {
        match self.credits_with_etag(tmdb_id, kind, None).await? {
            TmdbCreditsFetch::Fresh { credits, .. } => Ok(credits.cast),
            // Unreachable: `prior_etag = None` means the server cannot
            // legally produce a 304. Treat as decode error in case a
            // misbehaving mock yields one.
            TmdbCreditsFetch::NotModified => Err(Error::Decode(
                "tmdb credits: 304 returned without prior etag".to_string(),
            )),
        }
    }

    /// `ETag`-aware variant of [`credits`](Self::credits) for cache
    /// revalidation (PRD §F-003). Pass the cache row's stored `ETag` as
    /// `prior_etag`; on `304 Not Modified` the server confirms the cached
    /// cast list is still current and the caller re-uses it (refreshing
    /// the row's `expires_at`). On a fresh `2xx`, the parsed `ETag` header
    /// is returned alongside the new payload for persistence.
    pub async fn credits_with_etag(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        prior_etag: Option<&str>,
    ) -> Result<TmdbCreditsFetch, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}/credits", self.base_url);
        let outcome = fetch_with_etag(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            prior_etag,
            &self.config,
        )
        .await?;
        let (response, etag) = match outcome {
            FetchOutcome::NotModified => return Ok(TmdbCreditsFetch::NotModified),
            FetchOutcome::Fresh { response, etag } => (response, etag),
        };
        let body: CreditsResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media} credits: {e}")))?;
        let cast = body
            .cast
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| TmdbCastMember {
                name: c.name,
                character: c.character.filter(|s| !s.is_empty()),
                photo_url: c.profile_path.as_deref().map(tmdb_profile_url),
            })
            .collect();
        Ok(TmdbCreditsFetch::Fresh {
            credits: TmdbCredits {
                tmdb_id,
                kind,
                cast,
            },
            etag,
        })
    }

    /// Multi-search across movies and TV shows (PRD §F-011).
    ///
    /// Calls `/3/search/multi?query=...&language=...&page=...`. TMDB returns
    /// at most 20 items per page; F-011 ships infinite scroll at the same
    /// 20-per-page granularity so the caller can forward `page` directly
    /// from the UI. People-results (TMDB also serves person rows on this
    /// endpoint) are filtered out so the home-screen / search UI only sees
    /// movies and series.
    ///
    /// `locale` is the BCP-47 language tag forwarded to TMDB's `language`
    /// parameter so titles come back localized when possible. Pass
    /// `"en-US"` for the default.
    pub async fn search_multi(
        &self,
        query: &str,
        locale: &str,
        page: u32,
    ) -> Result<Vec<TitleSummary>, Error> {
        let url = format!("{}/3/search/multi", self.base_url);
        let page_str = page.max(1).to_string();
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("query", query),
                    ("language", locale),
                    ("page", page_str.as_str()),
                    ("include_adult", "false"),
                ])
            },
            &self.config,
        )
        .await?;
        let body: SearchMultiResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb search multi: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .filter_map(SearchMultiResult::into_summary)
            .collect())
    }

    async fn fetch_trending(
        &self,
        media_type: &'static str,
        locale: &str,
        kind: TitleKind,
    ) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/3/trending/{media_type}/week", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str()), ("language", locale)])
            },
            &self.config,
        )
        .await?;
        let body: TrendingResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb trending {media_type}: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, item)| item.into_provider(kind, rank))
            .collect())
    }
}

/// Shape of the `/3/trending/{type}/week` response, narrowed to the fields
/// F-004 cares about. Unknown fields are ignored by serde so TMDB schema
/// growth doesn't break us.
#[derive(Debug, Deserialize)]
struct TrendingResponse {
    results: Vec<TrendingItem>,
}

#[derive(Debug, Deserialize)]
struct TrendingItem {
    id: u64,
    /// Movies use `title`; TV uses `name`. We accept whichever the response
    /// carries via the two optional fields and synthesize the displayable
    /// string below.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Date strings; movies use `release_date`, TV uses `first_air_date`.
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    popularity: Option<f64>,
}

impl TrendingItem {
    fn into_provider(self, kind: TitleKind, rank: usize) -> ProviderItem {
        let title = self.title.or(self.name).unwrap_or_default();
        let year = self
            .release_date
            .as_deref()
            .or(self.first_air_date.as_deref())
            .and_then(parse_year);
        let poster = self.poster_path.as_deref().map(tmdb_poster_url);
        let summary = TitleSummary {
            id: format!("tmdb:{}", self.id),
            kind,
            title,
            year,
            poster,
            rating: self.vote_average,
        };
        ProviderItem {
            summary,
            rank,
            popularity: self.popularity,
            rating: self.vote_average,
        }
    }
}

/// Parse a leading `YYYY` from an ISO-8601 date string. TMDB returns dates
/// as `YYYY-MM-DD`; some catalog rows have an empty string. Out-of-range
/// values (negative years, etc.) yield `None`.
fn parse_year(date: &str) -> Option<u16> {
    let year_str = date.split('-').next().unwrap_or("");
    if year_str.len() != 4 {
        return None;
    }
    year_str.parse::<u16>().ok()
}

/// Build a TMDB poster URL at the `w500` size. The F-005 image resolver will
/// replace this with the proper provider-fallback chain; until then the
/// catalog UI gets a useful poster placeholder straight from the trending
/// fetch.
fn tmdb_poster_url(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    format!("https://image.tmdb.org/t/p/w500/{trimmed}")
}

/// Build a TMDB image URL at the `original` size for the F-005 resolver.
/// The catalog UI scales down; using `original` lets the renderer pick the
/// right tier on a per-display basis without re-fetching.
fn tmdb_artwork_url(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    format!("https://image.tmdb.org/t/p/original/{trimmed}")
}

/// Build the `include_image_language` query string from the user's language
/// chain plus `null` (textless artwork). TMDB accepts a comma-separated list
/// of 2-letter codes; `null` is a sentinel for "no language tag".
fn include_image_language(lang_pref: &[String]) -> String {
    let mut parts: Vec<String> = lang_pref.iter().map(|s| normalize_for_tmdb(s)).collect();
    parts.push("null".to_string());
    // Dedup while preserving order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    parts.retain(|s: &String| seen.insert(s.clone()));
    parts.join(",")
}

/// Convert a BCP-47-ish lang ("en", "en-US") into TMDB's expected 2-letter
/// code. ISO 639-2 inputs ("eng", "fre") fall through to a 3-letter form
/// which TMDB tolerates by silently ignoring.
fn normalize_for_tmdb(lang: &str) -> String {
    let primary = lang.split(['-', '_']).next().unwrap_or("");
    primary.to_ascii_lowercase()
}

fn into_asset(img: ImageEntry) -> LocalizedAsset {
    LocalizedAsset {
        lang: img.iso_639_1.unwrap_or_default(),
        url: tmdb_artwork_url(&img.file_path),
    }
}

#[derive(Debug, Deserialize)]
struct FindResponse {
    #[serde(default)]
    movie_results: Vec<FindResultEntry>,
    #[serde(default)]
    tv_results: Vec<FindResultEntry>,
}

#[derive(Debug, Deserialize)]
struct FindResultEntry {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct ExternalIdsResponse {
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    tvdb_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    posters: Vec<ImageEntry>,
    #[serde(default)]
    backdrops: Vec<ImageEntry>,
    #[serde(default)]
    logos: Vec<ImageEntry>,
}

#[derive(Debug, Deserialize)]
struct ImageEntry {
    file_path: String,
    /// Empty / null means textless (provider-neutral artwork).
    #[serde(default)]
    iso_639_1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DetailResponse {
    #[serde(default)]
    overview: Option<String>,
}

/// Shape of `/3/{movie,tv}/{id}?append_to_response=...` narrowed to the
/// fields F-010 reads. Unknown fields stay ignored (serde default) so
/// TMDB schema growth doesn't break this client.
#[derive(Debug, Deserialize)]
struct TitleDetailResponse {
    #[serde(default)]
    overview: Option<String>,
    /// Movies only. Minutes.
    #[serde(default)]
    runtime: Option<u32>,
    /// TV only. Minutes per episode; TMDB sometimes carries an empty
    /// array, a single entry, or a list (one per season).
    #[serde(default)]
    episode_run_time: Vec<u32>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    genres: Vec<GenreEntry>,
    /// Movies only — present when `append_to_response=release_dates`.
    #[serde(default)]
    release_dates: Option<ReleaseDatesEnvelope>,
    /// TV only — present when `append_to_response=content_ratings`.
    #[serde(default)]
    content_ratings: Option<ContentRatingsEnvelope>,
}

#[derive(Debug, Deserialize)]
struct GenreEntry {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseDatesEnvelope {
    #[serde(default)]
    results: Vec<ReleaseDatesEntry>,
}

#[derive(Debug, Deserialize)]
struct ReleaseDatesEntry {
    #[serde(default)]
    iso_3166_1: String,
    #[serde(default)]
    release_dates: Vec<ReleaseDateRow>,
}

#[derive(Debug, Deserialize)]
struct ReleaseDateRow {
    #[serde(default)]
    certification: String,
}

#[derive(Debug, Deserialize)]
struct ContentRatingsEnvelope {
    #[serde(default)]
    results: Vec<ContentRatingEntry>,
}

#[derive(Debug, Deserialize)]
struct ContentRatingEntry {
    #[serde(default)]
    iso_3166_1: String,
    #[serde(default)]
    rating: String,
}

/// Pick the first non-empty `release_dates[].certification` for the
/// `US` region from TMDB's movie release-dates envelope. Other regions
/// are intentionally ignored — the PRD §F-010 "age rating" is a single
/// label; picking a single locked region keeps the UX consistent across
/// titles. Future polish could fall back to the user's locale region
/// when US has no rating.
fn pick_us_certification_movie(entries: &[ReleaseDatesEntry]) -> Option<String> {
    entries
        .iter()
        .find(|e| e.iso_3166_1.eq_ignore_ascii_case("US"))
        .and_then(|e| {
            e.release_dates
                .iter()
                .find(|r| !r.certification.is_empty())
                .map(|r| r.certification.clone())
        })
}

/// Pick the US `content_ratings.results[].rating` for a TV title.
fn pick_us_certification_show(entries: &[ContentRatingEntry]) -> Option<String> {
    entries
        .iter()
        .find(|e| e.iso_3166_1.eq_ignore_ascii_case("US") && !e.rating.is_empty())
        .map(|e| e.rating.clone())
}

/// Pure-function projection from the raw `/3/{movie,tv}/{id}` body to the
/// public [`TmdbTitleDetails`] shape. Factored out so both
/// [`TmdbClient::title_details`] and
/// [`TmdbClient::title_details_with_etag`] can share the parser without
/// duplicating the field-by-field plucking logic.
fn parse_title_details(
    body: TitleDetailResponse,
    tmdb_id: u64,
    kind: TitleKind,
    language: &str,
) -> TmdbTitleDetails {
    let age_rating = match kind {
        TitleKind::Movie => body
            .release_dates
            .and_then(|r| pick_us_certification_movie(&r.results)),
        TitleKind::Series => body
            .content_ratings
            .and_then(|r| pick_us_certification_show(&r.results)),
    };
    let runtime_minutes = body
        .runtime
        .or_else(|| body.episode_run_time.iter().copied().find(|n| *n > 0));
    TmdbTitleDetails {
        tmdb_id,
        kind,
        language: language.to_string(),
        runtime_minutes,
        age_rating,
        genres: body
            .genres
            .into_iter()
            .map(|g| g.name)
            .filter(|s| !s.is_empty())
            .collect(),
        overview: body.overview.filter(|s| !s.is_empty()),
        rating: body.vote_average.filter(|n| *n > 0.0),
    }
}

/// Shape of `/3/search/multi`. TMDB returns the union of movie / tv / person
/// rows; `media_type` is the discriminator. Person rows lack a `kind` we can
/// surface so [`SearchMultiResult::into_summary`] drops them.
#[derive(Debug, Deserialize)]
struct SearchMultiResponse {
    #[serde(default)]
    results: Vec<SearchMultiResult>,
}

#[derive(Debug, Deserialize)]
struct SearchMultiResult {
    id: u64,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
}

impl SearchMultiResult {
    fn into_summary(self) -> Option<TitleSummary> {
        let kind = match self.media_type.as_deref()? {
            "movie" => TitleKind::Movie,
            "tv" => TitleKind::Series,
            _ => return None,
        };
        let title = self.title.or(self.name)?;
        if title.is_empty() {
            return None;
        }
        let year = self
            .release_date
            .as_deref()
            .or(self.first_air_date.as_deref())
            .and_then(parse_year);
        let poster = self.poster_path.as_deref().map(tmdb_poster_url);
        Some(TitleSummary {
            id: format!("tmdb:{}", self.id),
            kind,
            title,
            year,
            poster,
            rating: self.vote_average,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CreditsResponse {
    #[serde(default)]
    cast: Vec<CreditEntry>,
}

#[derive(Debug, Deserialize)]
struct CreditEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    character: Option<String>,
    #[serde(default)]
    profile_path: Option<String>,
}

/// Build a TMDB profile-photo URL at the `w185` size — TMDB's lowest
/// "human-readable" portrait tier, plenty for the 10-foot cast row.
fn tmdb_profile_url(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    format!("https://image.tmdb.org/t/p/w185/{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{header, header_regex, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config_zero_backoff() -> HttpConfig {
        HttpConfig::for_test()
    }

    #[tokio::test]
    async fn test_credentials_happy_path_sends_api_key_and_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .and(query_param("api_key", "test-key"))
            .and(header_regex(
                "user-agent",
                r"^kino/[\w\.\-\+]+ \(\+https?://[^)]+\)$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"images": {}})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_retries_on_429_then_succeeds() {
        let server = MockServer::start().await;
        // First call returns 429; subsequent calls return 200.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_retries_on_500_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_returns_error_after_exhausted_retries() {
        let server = MockServer::start().await;
        // 4 attempts (1 initial + 3 retries) all return 500.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(4)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("boom"));
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_credentials_returns_error_on_timeout_exhausted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .expect(4)
            .mount(&server)
            .await;

        let config = HttpConfig {
            timeout: Duration::from_millis(50),
            backoff: vec![Duration::ZERO; 3],
            ..HttpConfig::default()
        };
        let client = TmdbClient::with_options("test-key", config, server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        assert!(
            matches!(err, Error::Network(_)),
            "expected Network/timeout error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn trending_movies_parses_results_and_assigns_ranks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/movie/week"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("language", "en-US"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "id": 1,
                        "title": "First",
                        "release_date": "2025-04-01",
                        "poster_path": "/p1.jpg",
                        "vote_average": 8.4,
                        "popularity": 120.5,
                    },
                    {
                        "id": 2,
                        "title": "Second",
                        "release_date": "",
                        "poster_path": "",
                        "vote_average": 7.0,
                        "popularity": 80.0,
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_movies("en-US").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rank, 0);
        assert_eq!(items[0].summary.id, "tmdb:1");
        assert_eq!(items[0].summary.title, "First");
        assert_eq!(items[0].summary.year, Some(2025));
        assert_eq!(
            items[0].summary.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/p1.jpg")
        );
        assert_eq!(items[0].rating, Some(8.4));
        assert_eq!(items[0].popularity, Some(120.5));

        assert_eq!(items[1].rank, 1);
        assert_eq!(items[1].summary.year, None);
        // Empty poster_path normalizes to a URL too — the F-005 resolver
        // will replace it, but we don't drop the entry.
        assert!(items[1].summary.poster.is_some());
    }

    #[tokio::test]
    async fn trending_shows_uses_tv_endpoint_and_name_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/tv/week"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": 42,
                    "name": "Show",
                    "first_air_date": "2024-09-15",
                    "vote_average": 9.1,
                    "popularity": 220.0,
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_shows("en-US").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary.title, "Show");
        assert_eq!(items[0].summary.year, Some(2024));
        assert_eq!(items[0].summary.kind, TitleKind::Series);
    }

    #[tokio::test]
    async fn trending_caps_results_at_100() {
        let mut results = Vec::new();
        for i in 0..150 {
            results.push(json!({
                "id": i,
                "title": format!("T{i}"),
                "release_date": "2020-01-01",
                "vote_average": 5.0,
                "popularity": 10.0,
            }));
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/movie/week"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": results })))
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_movies("en-US").await.unwrap();
        assert_eq!(items.len(), 100);
    }

    #[tokio::test]
    async fn test_credentials_does_not_retry_on_401() {
        let server = MockServer::start().await;
        // Exactly one attempt — 4xx other than 429 must not retry.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .expect(1)
            .mount(&server)
            .await;

        let client = TmdbClient::with_options("nope", config_zero_backoff(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn find_external_resolves_imdb_to_tmdb_movie_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt0133093"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("external_source", "imdb_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "movie_results": [{ "id": 603 }],
                "tv_results": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let id = client
            .find_external("tt0133093", "imdb_id", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(id, Some(603));
    }

    #[tokio::test]
    async fn find_external_returns_none_when_no_match() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt9999999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "movie_results": [],
                "tv_results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let id = client
            .find_external("tt9999999", "imdb_id", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn external_ids_returns_full_id_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/external_ids"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "imdb_id": "tt0133093",
                "tvdb_id": 7782
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client.external_ids(603, TitleKind::Movie).await.unwrap();
        assert_eq!(ids.tmdb_id, Some(603));
        assert_eq!(ids.imdb_id.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tvdb_id, Some(7782));
    }

    #[tokio::test]
    async fn external_ids_handles_empty_imdb_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399/external_ids"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "imdb_id": "",
                "tvdb_id": null
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client.external_ids(1399, TitleKind::Series).await.unwrap();
        assert_eq!(ids.imdb_id, None);
        assert_eq!(ids.tvdb_id, None);
    }

    #[tokio::test]
    async fn artwork_images_parses_posters_backdrops_logos() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/images"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("include_image_language", "en,fr,null"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "posters": [
                    {"file_path": "/poster-en.jpg", "iso_639_1": "en"},
                    {"file_path": "/poster-fr.jpg", "iso_639_1": "fr"},
                    {"file_path": "/poster-textless.jpg", "iso_639_1": null}
                ],
                "backdrops": [
                    {"file_path": "/back-en.jpg", "iso_639_1": "en"}
                ],
                "logos": [
                    {"file_path": "/logo-en.png", "iso_639_1": "en"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .artwork_images(603, TitleKind::Movie, &["en".to_string(), "fr".to_string()])
            .await
            .unwrap();
        assert_eq!(bundle.posters.len(), 3);
        assert_eq!(bundle.posters[0].lang, "en");
        assert_eq!(bundle.posters[2].lang, "");
        assert_eq!(
            bundle.posters[0].url,
            "https://image.tmdb.org/t/p/original/poster-en.jpg"
        );
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.logos.len(), 1);
        // TMDB has no clearart endpoint; bucket stays empty.
        assert!(bundle.clearart.is_empty());
        assert!(bundle.summaries.is_empty());
    }

    #[tokio::test]
    async fn summary_returns_text_for_requested_language() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("language", "fr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "overview": "Néo découvre la matrice."
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let text = client.summary(603, TitleKind::Movie, "fr").await.unwrap();
        assert_eq!(text.as_deref(), Some("Néo découvre la matrice."));
    }

    #[tokio::test]
    async fn summary_returns_none_when_overview_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "overview": ""
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let text = client.summary(603, TitleKind::Movie, "ja").await.unwrap();
        assert!(text.is_none());
    }

    // ---- F-010: title_details & credits ----

    #[tokio::test]
    async fn title_details_parses_movie_payload_with_us_certification() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("language", "en"))
            .and(query_param("append_to_response", "release_dates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 603,
                "title": "The Matrix",
                "overview": "A computer hacker learns from mysterious rebels.",
                "runtime": 136,
                "vote_average": 8.2,
                "genres": [
                    {"id": 28, "name": "Action"},
                    {"id": 878, "name": "Science Fiction"}
                ],
                "release_dates": {
                    "results": [
                        {
                            "iso_3166_1": "FR",
                            "release_dates": [{"certification": "16"}]
                        },
                        {
                            "iso_3166_1": "US",
                            "release_dates": [
                                {"certification": ""},
                                {"certification": "R"}
                            ]
                        }
                    ]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let details = client
            .title_details(603, TitleKind::Movie, "en")
            .await
            .unwrap();
        assert_eq!(details.runtime_minutes, Some(136));
        assert_eq!(details.age_rating.as_deref(), Some("R"));
        assert_eq!(details.genres, vec!["Action", "Science Fiction"]);
        assert_eq!(
            details.overview.as_deref(),
            Some("A computer hacker learns from mysterious rebels.")
        );
        assert_eq!(details.rating, Some(8.2));
    }

    #[tokio::test]
    async fn title_details_uses_episode_run_time_for_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399"))
            .and(query_param("append_to_response", "content_ratings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1399,
                "name": "Game of Thrones",
                "overview": "Seven noble families fight.",
                "episode_run_time": [60, 55],
                "vote_average": 8.4,
                "genres": [{"id": 10765, "name": "Sci-Fi & Fantasy"}],
                "content_ratings": {
                    "results": [
                        {"iso_3166_1": "GB", "rating": "18"},
                        {"iso_3166_1": "US", "rating": "TV-MA"}
                    ]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let details = client
            .title_details(1399, TitleKind::Series, "en")
            .await
            .unwrap();
        assert_eq!(details.runtime_minutes, Some(60));
        assert_eq!(details.age_rating.as_deref(), Some("TV-MA"));
        assert_eq!(details.genres, vec!["Sci-Fi & Fantasy"]);
        assert_eq!(details.rating, Some(8.4));
    }

    #[tokio::test]
    async fn title_details_handles_missing_optional_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 42,
                "title": "Sparse",
                "overview": "",
                "vote_average": 0.0
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let details = client
            .title_details(42, TitleKind::Movie, "en")
            .await
            .unwrap();
        assert!(details.runtime_minutes.is_none());
        assert!(details.age_rating.is_none());
        assert!(details.genres.is_empty());
        assert!(details.overview.is_none());
        assert!(details.rating.is_none());
    }

    #[tokio::test]
    async fn title_details_with_etag_no_prior_returns_fresh_with_server_etag() {
        // PRD §F-003: ETag handled where the provider supports it. On the
        // first fetch (no prior cache row) the caller passes `prior_etag =
        // None` and gets back the `ETag` header for persistence.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(query_param("api_key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"matrix-v1\"")
                    .set_body_json(json!({
                        "id": 603,
                        "title": "The Matrix",
                        "runtime": 136,
                        "vote_average": 8.2
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_details_with_etag(603, TitleKind::Movie, "en", None)
            .await
            .unwrap();
        match fetch {
            TmdbTitleDetailsFetch::Fresh { details, etag } => {
                assert_eq!(etag.as_deref(), Some("\"matrix-v1\""));
                assert_eq!(details.runtime_minutes, Some(136));
                assert_eq!(details.rating, Some(8.2));
            }
            TmdbTitleDetailsFetch::NotModified => panic!("expected Fresh on first fetch"),
        }
    }

    #[tokio::test]
    async fn title_details_with_etag_prior_sends_if_none_match_and_304_yields_not_modified() {
        // PRD §F-003 304 path: a server that confirms the cached row is
        // current returns 304, and we surface NotModified so the caller
        // refreshes the row's `expires_at` and re-uses its parsed payload.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399"))
            .and(query_param("api_key", "test-key"))
            .and(header("if-none-match", "\"got-v2\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_details_with_etag(1399, TitleKind::Series, "en", Some("\"got-v2\""))
            .await
            .unwrap();
        assert!(matches!(fetch, TmdbTitleDetailsFetch::NotModified));
    }

    #[tokio::test]
    async fn title_details_with_etag_prior_with_changed_resource_returns_fresh_with_new_etag() {
        // Server's resource changed since we cached: it ignores our
        // `If-None-Match` and returns a fresh 200 with a new ETag.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(header("if-none-match", "\"matrix-v1\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"matrix-v2\"")
                    .set_body_json(json!({
                        "id": 603,
                        "title": "The Matrix",
                        "runtime": 137,
                        "vote_average": 8.3
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_details_with_etag(603, TitleKind::Movie, "en", Some("\"matrix-v1\""))
            .await
            .unwrap();
        match fetch {
            TmdbTitleDetailsFetch::Fresh { details, etag } => {
                assert_eq!(etag.as_deref(), Some("\"matrix-v2\""));
                assert_eq!(details.runtime_minutes, Some(137));
            }
            TmdbTitleDetailsFetch::NotModified => panic!("expected Fresh on changed resource"),
        }
    }

    #[tokio::test]
    async fn title_details_with_etag_tolerates_missing_etag_header() {
        // PRD §F-003: ETag is "handled where the provider supports it" —
        // when the server simply doesn't send an ETag (some Fanart-class
        // endpoints), `etag` is `None` and the caller persists it as such.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 603,
                "title": "The Matrix",
                "runtime": 136,
                "vote_average": 8.2
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_details_with_etag(603, TitleKind::Movie, "en", None)
            .await
            .unwrap();
        match fetch {
            TmdbTitleDetailsFetch::Fresh { etag, .. } => assert!(etag.is_none()),
            TmdbTitleDetailsFetch::NotModified => panic!("expected Fresh"),
        }
    }

    #[tokio::test]
    async fn title_details_back_compat_unchanged_when_server_sends_etag() {
        // Sanity: the back-compat `title_details` wrapper still returns
        // the bare struct even when the server sends an ETag (the new
        // metadata is invisible to existing callers).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"matrix-v1\"")
                    .set_body_json(json!({
                        "id": 603,
                        "title": "The Matrix",
                        "runtime": 136,
                        "vote_average": 8.2
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let details = client
            .title_details(603, TitleKind::Movie, "en")
            .await
            .unwrap();
        assert_eq!(details.runtime_minutes, Some(136));
        assert_eq!(details.rating, Some(8.2));
    }

    #[tokio::test]
    async fn credits_returns_cast_with_photo_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/credits"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cast": [
                    {
                        "name": "Keanu Reeves",
                        "character": "Neo",
                        "profile_path": "/4D0PpNI0kmP58hgrwGC3wCjxhnm.jpg"
                    },
                    {
                        "name": "Laurence Fishburne",
                        "character": "Morpheus",
                        "profile_path": null
                    },
                    {
                        "name": "",
                        "character": "Crew",
                        "profile_path": null
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let cast = client.credits(603, TitleKind::Movie).await.unwrap();
        // Empty-name cast entry is filtered out.
        assert_eq!(cast.len(), 2);
        assert_eq!(cast[0].name, "Keanu Reeves");
        assert_eq!(cast[0].character.as_deref(), Some("Neo"));
        assert_eq!(
            cast[0].photo_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w185/4D0PpNI0kmP58hgrwGC3wCjxhnm.jpg")
        );
        assert_eq!(cast[1].name, "Laurence Fishburne");
        assert!(cast[1].photo_url.is_none());
    }

    #[tokio::test]
    async fn credits_returns_empty_when_cast_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/credits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let cast = client.credits(603, TitleKind::Movie).await.unwrap();
        assert!(cast.is_empty());
    }

    #[tokio::test]
    async fn credits_with_etag_no_prior_returns_fresh_with_server_etag() {
        // PRD §F-003 round-trip on TMDB `/credits`: the first fetch (no
        // prior cache row) yields a fresh cast list AND the server's ETag
        // so the next call can revalidate.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/credits"))
            .and(query_param("api_key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"cast-v1\"")
                    .set_body_json(json!({
                        "cast": [
                            {
                                "name": "Keanu Reeves",
                                "character": "Neo",
                                "profile_path": "/k.jpg"
                            }
                        ]
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .credits_with_etag(603, TitleKind::Movie, None)
            .await
            .unwrap();
        match fetch {
            TmdbCreditsFetch::Fresh { credits, etag } => {
                assert_eq!(etag.as_deref(), Some("\"cast-v1\""));
                assert_eq!(credits.tmdb_id, 603);
                assert_eq!(credits.kind, TitleKind::Movie);
                assert_eq!(credits.cast.len(), 1);
                assert_eq!(credits.cast[0].name, "Keanu Reeves");
            }
            TmdbCreditsFetch::NotModified => panic!("expected Fresh on first fetch"),
        }
    }

    #[tokio::test]
    async fn credits_with_etag_prior_sends_if_none_match_and_304_yields_not_modified() {
        // PRD §F-003 304 cache-hit path on `/credits`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399/credits"))
            .and(query_param("api_key", "test-key"))
            .and(header("if-none-match", "\"got-cast-v2\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .credits_with_etag(1399, TitleKind::Series, Some("\"got-cast-v2\""))
            .await
            .unwrap();
        assert!(matches!(fetch, TmdbCreditsFetch::NotModified));
    }

    #[tokio::test]
    async fn credits_back_compat_unchanged_when_server_sends_etag() {
        // The back-compat `credits` wrapper still returns the bare
        // `Vec<TmdbCastMember>` even when the server sends an ETag.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/credits"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"cast-v1\"")
                    .set_body_json(json!({
                        "cast": [{ "name": "Keanu Reeves", "character": "Neo" }]
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let cast = client.credits(603, TitleKind::Movie).await.unwrap();
        assert_eq!(cast.len(), 1);
        assert_eq!(cast[0].name, "Keanu Reeves");
    }

    #[tokio::test]
    async fn search_multi_returns_movie_and_tv_rows_drops_people() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("query", "matrix"))
            .and(query_param("language", "en-US"))
            .and(query_param("page", "1"))
            .and(query_param("include_adult", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "id": 603,
                        "media_type": "movie",
                        "title": "The Matrix",
                        "release_date": "1999-03-31",
                        "poster_path": "/p.jpg",
                        "vote_average": 8.2
                    },
                    {
                        "id": 1399,
                        "media_type": "tv",
                        "name": "Game of Thrones",
                        "first_air_date": "2011-04-17",
                        "poster_path": "/g.jpg",
                        "vote_average": 8.4
                    },
                    {
                        "id": 999,
                        "media_type": "person",
                        "name": "Keanu Reeves"
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search_multi("matrix", "en-US", 1).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tmdb:603");
        assert_eq!(items[0].kind, TitleKind::Movie);
        assert_eq!(items[0].title, "The Matrix");
        assert_eq!(items[0].year, Some(1999));
        assert_eq!(
            items[0].poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/p.jpg")
        );
        assert_eq!(items[1].id, "tmdb:1399");
        assert_eq!(items[1].kind, TitleKind::Series);
        assert_eq!(items[1].title, "Game of Thrones");
        assert_eq!(items[1].year, Some(2011));
    }

    #[tokio::test]
    async fn search_multi_forwards_page_number_to_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .and(query_param("page", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search_multi("zzz", "en", 3).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_multi_coerces_zero_page_to_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search_multi("zzz", "en", 0).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_multi_drops_rows_with_empty_title() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/search/multi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": 1, "media_type": "movie", "title": "", "release_date": "2020" },
                    { "id": 2, "media_type": "movie", "title": "Real", "release_date": "" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search_multi("q", "en", 1).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Real");
        assert!(items[0].year.is_none());
    }
}
