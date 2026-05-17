//! Image & summary resolution (PRD §F-005).
//!
//! Each title displayed in the UI carries a poster, backdrop, optional logo,
//! optional clearart, and a summary text. PRD §F-005 locks a six-tier
//! fallback algorithm: within each language tier, providers are tried in
//! a fixed order; if no provider has an asset at the requested language, the
//! next language tier is tried; the final tier is a local placeholder asset.
//!
//! ## Algorithm (locked)
//!
//! For each image kind (poster / backdrop / logo / clearart) independently:
//!
//! 1. Tier 1 — primary language: Fanart.tv → TMDB → TVDB
//! 2. Tier 2 — first configured fallback language: Fanart.tv → TMDB → TVDB
//! 3. Tier 3 — second configured fallback language: Fanart.tv → TMDB → TVDB
//! 4. Tier 4 — third configured fallback language: Fanart.tv → TMDB → TVDB
//! 5. Tier 5 — any other language available per provider: Fanart.tv → TMDB → TVDB
//! 6. Tier 6 — local placeholder asset shipped with the app
//!
//! The first non-empty asset returned wins for that image type. Summary text
//! follows the same tier structure but skips Fanart.tv (which does not
//! serve summary text).
//!
//! A provider with no API key configured is skipped entirely (treated as
//! "no asset"); the chain continues with the next provider. Failures from
//! a provider's network call surface as "no asset" too — the UI must never
//! crash because Fanart.tv was down.
//!
//! ## Caching
//!
//! Resolved [`Artwork`] payloads are cached per `(title_id, kind, lang_chain_hash)`
//! by the Tauri host command. The TTL is locked at
//! [`kino_core::constants::ARTWORK_TTL_S`] (7 days). The `lang_chain_hash`
//! depends on the full `lang_pref` so a settings change invalidates stale
//! rows on the next read.
//!
//! ## ID resolution
//!
//! Each provider keys on a different external id: Fanart.tv movies use
//! `IMDb`; Fanart.tv TV uses `TVDB`; TMDB uses `TMDB`; TVDB uses `TVDB`.
//! The host command parses the catalog `title_id` (e.g. `tmdb:603`,
//! `tt0133093`, `tvdb:78878`) into a [`TitleIds`] bag before calling
//! [`resolve`]. A provider is skipped if the bag doesn't carry its id;
//! cross-id enrichment is best-effort via the optional TMDB `find` / TMDB
//! `external_ids` calls.

use std::collections::HashSet;

use kino_core::title::TitleKind;
use serde::{Deserialize, Serialize};

use crate::{FanartClient, TmdbClient, TvdbClient};

/// External-id bag for a title. Populated by the host command from a parsed
/// catalog id, optionally enriched by a TMDB lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleIds {
    /// `IMDb` id (e.g. `tt0133093`). Required for Fanart.tv movie lookups.
    pub imdb: Option<String>,
    /// TMDB numeric id. Required for TMDB image / summary lookups.
    pub tmdb: Option<u64>,
    /// TVDB numeric id. Required for TVDB lookups and Fanart.tv TV lookups.
    pub tvdb: Option<u64>,
}

/// The four image kinds the UI can render. Each is resolved independently
/// of the others per PRD §F-005.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtKind {
    Poster,
    Backdrop,
    Logo,
    Clearart,
}

/// A single image-or-text artifact paired with its provenance.
///
/// `source` is a human-readable tag like `"fanart.tv:en"`, `"tmdb:fr"`,
/// `"tvdb:eng"`, `"placeholder"`, or `""` (for an empty summary fallback).
/// PRD §F-005 calls it out explicitly for debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtField {
    /// Resolved URL. For tier 6 falls back to a `placeholder:<kind>` scheme
    /// that the frontend renders against bundled assets.
    pub url: String,
    /// Provenance label.
    pub source: String,
}

/// Summary text + provenance. Empty `text` + empty `source` means no
/// provider had a summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryField {
    pub text: String,
    pub source: String,
}

/// The full resolved bundle returned by [`resolve`] and the
/// `resolve_artwork` Tauri command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artwork {
    pub poster: ArtField,
    pub backdrop: ArtField,
    pub logo: ArtField,
    pub clearart: ArtField,
    pub summary: SummaryField,
}

/// A language-tagged URL. `lang` is the normalized 2-letter ISO 639-1 code
/// (e.g. `"en"`, `"fr"`) or an empty string for language-agnostic art (TMDB
/// returns `null` `iso_639_1`; Fanart returns `"00"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangAsset {
    pub lang: String,
    pub url: String,
}

/// Language-tagged summary text. `lang` follows [`LangAsset::lang`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangText {
    pub lang: String,
    pub text: String,
}

/// Everything a provider can supply about a title's artwork.
///
/// Built by each provider's `fetch_art_bundle` method. The aggregator never
/// constructs these directly outside tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderArtBundle {
    pub posters: Vec<LangAsset>,
    pub backdrops: Vec<LangAsset>,
    pub logos: Vec<LangAsset>,
    pub clearart: Vec<LangAsset>,
    /// Per-language summary text. Fanart.tv leaves this empty.
    pub summaries: Vec<LangText>,
}

impl ProviderArtBundle {
    fn assets_for(&self, kind: ArtKind) -> &[LangAsset] {
        match kind {
            ArtKind::Poster => &self.posters,
            ArtKind::Backdrop => &self.backdrops,
            ArtKind::Logo => &self.logos,
            ArtKind::Clearart => &self.clearart,
        }
    }
}

/// Identifier used in the [`ArtField::source`] tag for each provider.
const FANART_SOURCE: &str = "fanart.tv";
const TMDB_SOURCE: &str = "tmdb";
const TVDB_SOURCE: &str = "tvdb";
const PLACEHOLDER_SOURCE: &str = "placeholder";

/// Sentinel `lang` value the resolver uses for the "any other language"
/// tier (PRD §F-005 tier 5). Compares equal to nothing in `lang_pref` so
/// the per-language match must be relaxed to "any first available" when
/// this tier is in play.
const ANY_LANG_TIER: &str = "*";

/// URL the resolver returns when every tier is exhausted (PRD §F-005 tier 6).
/// The frontend resolves the `placeholder:<kind>` scheme to a bundled asset
/// at render time so the resolver itself stays platform-independent.
fn placeholder_for(kind: ArtKind) -> String {
    match kind {
        ArtKind::Poster => "placeholder:poster".to_string(),
        ArtKind::Backdrop => "placeholder:backdrop".to_string(),
        ArtKind::Logo => "placeholder:logo".to_string(),
        ArtKind::Clearart => "placeholder:clearart".to_string(),
    }
}

/// Map TVDB's ISO 639-2/T 3-letter codes onto the 2-letter ISO 639-1 codes
/// the rest of the chain uses. Returns the input unchanged if it's already
/// a 2-letter code; returns an empty string for unknown 3-letter codes so
/// the asset still surfaces in the tier-5 "any language" walk.
fn normalize_lang(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    // Sentinel "language-agnostic" markers come in at length 2 (`"00"`) or
    // length 0 — collapse both to empty so the resolver's tier 5 sweep
    // picks them up.
    if lower == "00" || lower.is_empty() || lower == "xx" || lower == "null" {
        return String::new();
    }
    if lower.len() == 2 {
        return lower;
    }
    match lower.as_str() {
        "eng" => "en".to_string(),
        "fra" | "fre" => "fr".to_string(),
        "deu" | "ger" => "de".to_string(),
        "spa" => "es".to_string(),
        "ita" => "it".to_string(),
        "jpn" => "ja".to_string(),
        "kor" => "ko".to_string(),
        "rus" => "ru".to_string(),
        "por" => "pt".to_string(),
        "zho" | "chi" => "zh".to_string(),
        "nld" | "dut" => "nl".to_string(),
        "pol" => "pl".to_string(),
        "ara" => "ar".to_string(),
        "tur" => "tr".to_string(),
        "swe" => "sv".to_string(),
        "dan" => "da".to_string(),
        "fin" => "fi".to_string(),
        "nor" | "nob" | "nno" => "no".to_string(),
        "ces" | "cze" => "cs".to_string(),
        "ell" | "gre" => "el".to_string(),
        "heb" => "he".to_string(),
        "hin" => "hi".to_string(),
        "hun" => "hu".to_string(),
        "ind" => "id".to_string(),
        "tha" => "th".to_string(),
        "ukr" => "uk".to_string(),
        "vie" => "vi".to_string(),
        _ => String::new(),
    }
}

/// Resolve artwork and summary for a single title.
///
/// `lang_pref` carries the user's language tier list (1–4 entries). The
/// algorithm always appends an implicit tier-5 "any other language" sweep
/// followed by tier-6 placeholders, so callers don't pad the slice.
///
/// Each `Option<&ProviderArtBundle>` is the result of pre-fetching that
/// provider's art for the title. Passing `None` (e.g. because the API key
/// is missing or the per-provider fetch errored) skips that provider in
/// every tier — exactly as PRD §F-005 requires.
#[must_use]
#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
pub fn resolve(
    fanart: Option<&ProviderArtBundle>,
    tmdb: Option<&ProviderArtBundle>,
    tvdb: Option<&ProviderArtBundle>,
    lang_pref: &[String],
) -> Artwork {
    Artwork {
        poster: resolve_field(ArtKind::Poster, fanart, tmdb, tvdb, lang_pref),
        backdrop: resolve_field(ArtKind::Backdrop, fanart, tmdb, tvdb, lang_pref),
        logo: resolve_field(ArtKind::Logo, fanart, tmdb, tvdb, lang_pref),
        clearart: resolve_field(ArtKind::Clearart, fanart, tmdb, tvdb, lang_pref),
        summary: resolve_summary(tmdb, tvdb, lang_pref),
    }
}

#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
fn resolve_field(
    kind: ArtKind,
    fanart: Option<&ProviderArtBundle>,
    tmdb: Option<&ProviderArtBundle>,
    tvdb: Option<&ProviderArtBundle>,
    lang_pref: &[String],
) -> ArtField {
    // Tiers 1..=4 walk the user's configured languages; tier 5 walks each
    // provider's "any other language" pool.
    for lang in lang_pref
        .iter()
        .chain(std::iter::once(&ANY_LANG_TIER.to_string()))
    {
        if let Some(field) = pick_at_tier(kind, lang, fanart, FANART_SOURCE) {
            return field;
        }
        if let Some(field) = pick_at_tier(kind, lang, tmdb, TMDB_SOURCE) {
            return field;
        }
        if let Some(field) = pick_at_tier(kind, lang, tvdb, TVDB_SOURCE) {
            return field;
        }
    }
    ArtField {
        url: placeholder_for(kind),
        source: PLACEHOLDER_SOURCE.to_string(),
    }
}

fn pick_at_tier(
    kind: ArtKind,
    lang_tier: &str,
    bundle: Option<&ProviderArtBundle>,
    provider: &str,
) -> Option<ArtField> {
    let bundle = bundle?;
    let assets = bundle.assets_for(kind);
    let chosen = if lang_tier == ANY_LANG_TIER {
        assets.first()
    } else {
        let normalized_tier = lang_tier.to_ascii_lowercase();
        assets
            .iter()
            .find(|a| normalize_lang(&a.lang) == normalized_tier)
    };
    let asset = chosen?;
    if asset.url.is_empty() {
        return None;
    }
    let source_lang = if asset.lang.is_empty() {
        "*"
    } else {
        asset.lang.as_str()
    };
    Some(ArtField {
        url: asset.url.clone(),
        source: format!("{provider}:{source_lang}"),
    })
}

#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
fn resolve_summary(
    tmdb: Option<&ProviderArtBundle>,
    tvdb: Option<&ProviderArtBundle>,
    lang_pref: &[String],
) -> SummaryField {
    for lang in lang_pref
        .iter()
        .chain(std::iter::once(&ANY_LANG_TIER.to_string()))
    {
        if let Some(s) = pick_summary_at_tier(lang, tmdb, TMDB_SOURCE) {
            return s;
        }
        if let Some(s) = pick_summary_at_tier(lang, tvdb, TVDB_SOURCE) {
            return s;
        }
    }
    SummaryField {
        text: String::new(),
        source: String::new(),
    }
}

fn pick_summary_at_tier(
    lang_tier: &str,
    bundle: Option<&ProviderArtBundle>,
    provider: &str,
) -> Option<SummaryField> {
    let bundle = bundle?;
    let chosen = if lang_tier == ANY_LANG_TIER {
        bundle.summaries.iter().find(|s| !s.text.is_empty())
    } else {
        let normalized_tier = lang_tier.to_ascii_lowercase();
        bundle
            .summaries
            .iter()
            .find(|s| normalize_lang(&s.lang) == normalized_tier && !s.text.is_empty())
    };
    let entry = chosen?;
    let source_lang = if entry.lang.is_empty() {
        "*"
    } else {
        entry.lang.as_str()
    };
    Some(SummaryField {
        text: entry.text.clone(),
        source: format!("{provider}:{source_lang}"),
    })
}

/// Async one-shot that fetches each provider in parallel and resolves the
/// final [`Artwork`]. Each provider is optional; passing `None` skips that
/// provider entirely (per PRD §F-005 "A provider is skipped entirely if its
/// API key is not configured"). Per-provider fetch failures are demoted to
/// "no asset" via [`tracing::warn`] so the UI never crashes on an upstream
/// outage.
///
/// `lang_pref` carries the language tier list (1–4 user-configured langs
/// plus an implicit tier-5 sweep).
#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
pub async fn fetch_and_resolve(
    ids: &TitleIds,
    kind: TitleKind,
    lang_pref: &[String],
    fanart: Option<&FanartClient>,
    tmdb: Option<&TmdbClient>,
    tvdb: Option<&TvdbClient>,
) -> Artwork {
    let fanart_fut = async move {
        let client = fanart?;
        match kind {
            TitleKind::Movie => {
                let imdb = ids.imdb.as_deref()?;
                client.fetch_movie_art_bundle(imdb).await.ok()
            }
            TitleKind::Series => {
                let id = ids.tvdb?;
                client.fetch_show_art_bundle(id).await.ok()
            }
        }
    };
    let tmdb_fut = async move {
        let client = tmdb?;
        let id = ids.tmdb?;
        client.fetch_art_bundle(kind, id).await.ok()
    };
    let tvdb_fut = async move {
        let client = tvdb?;
        let id = ids.tvdb?;
        client.fetch_art_bundle(kind, id).await.ok()
    };

    let (fanart_bundle, tmdb_bundle, tvdb_bundle) = tokio::join!(fanart_fut, tmdb_fut, tvdb_fut);

    resolve(
        fanart_bundle.as_ref(),
        tmdb_bundle.as_ref(),
        tvdb_bundle.as_ref(),
        lang_pref,
    )
}

/// Stable hash of a normalized `lang_pref` slice, used by the host as part
/// of the `response_cache` key. Lower-cased to absorb harmless casing
/// differences; whitespace trimmed; entries that normalize to the same
/// 2-letter code collapse so `["EN", "en"]` hashes to the same key as
/// `["en"]`.
#[must_use]
pub fn lang_chain_hash(lang_pref: &[String]) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    let normalized: Vec<String> = lang_pref
        .iter()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    // Order-preserving dedup so `["EN", "en"]` and `["en"]` produce the
    // same hash, but `["en", "fr"]` and `["fr", "en"]` do not.
    let mut seen = HashSet::new();
    let mut ordered: Vec<&str> = Vec::with_capacity(normalized.len());
    for l in &normalized {
        if seen.insert(l.clone()) {
            ordered.push(l);
        }
    }
    let joined = ordered.join(",");
    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    let digest = hasher.finalize();
    // First 8 bytes as hex = 16 chars; plenty unique for a cache key.
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
#[allow(clippy::similar_names)] // `tmdb` / `tvdb` are PRD-locked provider names.
mod tests {
    use super::*;

    fn bundle_with(posters: Vec<(&str, &str)>) -> ProviderArtBundle {
        ProviderArtBundle {
            posters: posters
                .into_iter()
                .map(|(l, u)| LangAsset {
                    lang: l.to_string(),
                    url: u.to_string(),
                })
                .collect(),
            ..ProviderArtBundle::default()
        }
    }

    #[test]
    fn tier1_primary_language_picks_fanart_first() {
        let fanart = bundle_with(vec![("en", "fanart-en-poster.jpg")]);
        let tmdb = bundle_with(vec![("en", "tmdb-en-poster.jpg")]);
        let tvdb = bundle_with(vec![("en", "tvdb-en-poster.jpg")]);
        let art = resolve(Some(&fanart), Some(&tmdb), Some(&tvdb), &["en".to_string()]);
        assert_eq!(art.poster.url, "fanart-en-poster.jpg");
        assert_eq!(art.poster.source, "fanart.tv:en");
    }

    #[test]
    fn tier1_falls_through_to_tmdb_when_fanart_missing() {
        let tmdb = bundle_with(vec![("en", "tmdb-en-poster.jpg")]);
        let tvdb = bundle_with(vec![("en", "tvdb-en-poster.jpg")]);
        let art = resolve(None, Some(&tmdb), Some(&tvdb), &["en".to_string()]);
        assert_eq!(art.poster.url, "tmdb-en-poster.jpg");
        assert_eq!(art.poster.source, "tmdb:en");
    }

    #[test]
    fn tier1_falls_through_to_tvdb_when_fanart_and_tmdb_lack_lang() {
        let fanart = bundle_with(vec![("fr", "fanart-fr.jpg")]);
        let tmdb = bundle_with(vec![("fr", "tmdb-fr.jpg")]);
        let tvdb_bundle = ProviderArtBundle {
            posters: vec![LangAsset {
                lang: "eng".into(),
                url: "tvdb-eng.jpg".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let art = resolve(
            Some(&fanart),
            Some(&tmdb),
            Some(&tvdb_bundle),
            &["en".to_string()],
        );
        // Fanart has no "en"; TMDB has no "en"; TVDB has "eng" → "en".
        assert_eq!(art.poster.url, "tvdb-eng.jpg");
        assert_eq!(art.poster.source, "tvdb:eng");
    }

    #[test]
    fn tier2_kicks_in_when_tier1_has_no_match() {
        // Primary lang "ja" missing from every provider; secondary "en"
        // present on TMDB.
        let tmdb = bundle_with(vec![("en", "tmdb-en.jpg")]);
        let art = resolve(
            None,
            Some(&tmdb),
            None,
            &["ja".to_string(), "en".to_string()],
        );
        assert_eq!(art.poster.url, "tmdb-en.jpg");
        assert_eq!(art.poster.source, "tmdb:en");
    }

    #[test]
    fn tier5_any_language_used_when_no_configured_lang_matches() {
        // User wants ja then ko; only fr is available on TMDB.
        let tmdb = bundle_with(vec![("fr", "tmdb-fr.jpg")]);
        let art = resolve(
            None,
            Some(&tmdb),
            None,
            &["ja".to_string(), "ko".to_string()],
        );
        assert_eq!(art.poster.url, "tmdb-fr.jpg");
        assert_eq!(art.poster.source, "tmdb:fr");
    }

    #[test]
    fn tier6_placeholder_when_no_provider_has_anything() {
        let art = resolve(None, None, None, &["en".to_string()]);
        assert_eq!(art.poster.url, "placeholder:poster");
        assert_eq!(art.poster.source, "placeholder");
        assert_eq!(art.backdrop.url, "placeholder:backdrop");
        assert_eq!(art.logo.url, "placeholder:logo");
        assert_eq!(art.clearart.url, "placeholder:clearart");
        assert_eq!(art.summary.text, "");
        assert_eq!(art.summary.source, "");
    }

    #[test]
    fn per_image_type_independence_pulls_from_different_tiers() {
        // Poster from Fanart tier 1; logo only from TVDB tier 3.
        let fanart = ProviderArtBundle {
            posters: vec![LangAsset {
                lang: "en".into(),
                url: "fanart-poster-en.jpg".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let tvdb_bundle = ProviderArtBundle {
            logos: vec![LangAsset {
                lang: "deu".into(),
                url: "tvdb-logo-de.png".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let art = resolve(
            Some(&fanart),
            None,
            Some(&tvdb_bundle),
            &["en".to_string(), "fr".to_string(), "de".to_string()],
        );
        assert_eq!(art.poster.source, "fanart.tv:en");
        assert_eq!(art.backdrop.source, "placeholder");
        assert_eq!(art.logo.source, "tvdb:deu");
        assert_eq!(art.clearart.source, "placeholder");
    }

    #[test]
    fn summary_skips_fanart_and_walks_tmdb_then_tvdb() {
        // Fanart bundle includes a summary that the resolver must ignore
        // (Fanart doesn't actually serve summaries; this guards the
        // skip-Fanart rule even if a future change carries text on Fanart).
        let _fanart = ProviderArtBundle {
            summaries: vec![LangText {
                lang: "en".into(),
                text: "should be ignored".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let tmdb = ProviderArtBundle {
            summaries: vec![LangText {
                lang: "en".into(),
                text: "from tmdb".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let tvdb = ProviderArtBundle {
            summaries: vec![LangText {
                lang: "eng".into(),
                text: "from tvdb".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let art = resolve(None, Some(&tmdb), Some(&tvdb), &["en".to_string()]);
        assert_eq!(art.summary.text, "from tmdb");
        assert_eq!(art.summary.source, "tmdb:en");
    }

    #[test]
    fn summary_falls_through_to_tvdb_when_tmdb_empty() {
        let tmdb = ProviderArtBundle::default();
        let tvdb = ProviderArtBundle {
            summaries: vec![LangText {
                lang: "eng".into(),
                text: "tvdb summary".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let art = resolve(None, Some(&tmdb), Some(&tvdb), &["en".to_string()]);
        assert_eq!(art.summary.text, "tvdb summary");
        assert_eq!(art.summary.source, "tvdb:eng");
    }

    #[test]
    fn summary_empty_when_no_provider_serves_it() {
        let art = resolve(None, None, None, &["en".to_string()]);
        assert_eq!(art.summary.text, "");
        assert_eq!(art.summary.source, "");
    }

    #[test]
    fn provider_skip_when_key_missing_resolves_via_others() {
        // Fanart absent (missing key) but TMDB / TVDB still resolve.
        let tmdb = bundle_with(vec![("en", "tmdb-en.jpg")]);
        let tvdb_bundle = ProviderArtBundle {
            backdrops: vec![LangAsset {
                lang: "eng".into(),
                url: "tvdb-back.jpg".into(),
            }],
            ..ProviderArtBundle::default()
        };
        let art = resolve(None, Some(&tmdb), Some(&tvdb_bundle), &["en".to_string()]);
        assert_eq!(art.poster.source, "tmdb:en");
        assert_eq!(art.backdrop.source, "tvdb:eng");
    }

    #[test]
    fn empty_url_assets_are_skipped() {
        // Fanart returns an entry with the right lang but an empty URL —
        // resolver must fall through to TMDB rather than emit a broken
        // tile.
        let fanart = bundle_with(vec![("en", "")]);
        let tmdb = bundle_with(vec![("en", "tmdb-en.jpg")]);
        let art = resolve(Some(&fanart), Some(&tmdb), None, &["en".to_string()]);
        assert_eq!(art.poster.url, "tmdb-en.jpg");
        assert_eq!(art.poster.source, "tmdb:en");
    }

    #[test]
    fn normalize_lang_collapses_iso_639_2_to_2_letter() {
        assert_eq!(normalize_lang("eng"), "en");
        assert_eq!(normalize_lang("ENG"), "en");
        assert_eq!(normalize_lang("fra"), "fr");
        assert_eq!(normalize_lang("fre"), "fr");
        assert_eq!(normalize_lang("EN"), "en");
        assert_eq!(normalize_lang("00"), "");
        assert_eq!(normalize_lang(""), "");
        assert_eq!(normalize_lang("zzz"), "");
    }

    #[test]
    fn lang_chain_hash_is_stable_and_dedups() {
        let a = lang_chain_hash(&["en".to_string(), "fr".to_string()]);
        let b = lang_chain_hash(&["en".to_string(), "fr".to_string()]);
        let c = lang_chain_hash(&["EN".to_string(), "en".to_string(), "fr".to_string()]);
        let d = lang_chain_hash(&["fr".to_string(), "en".to_string()]);
        assert_eq!(a, b);
        assert_eq!(a, c, "casing + dedup must not affect the hash");
        assert_ne!(a, d, "order matters (tier priority is positional)");
        assert_eq!(a.len(), 16, "16 hex chars (8 bytes truncated)");
    }
}
