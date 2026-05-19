//! Image & logo resolution (PRD §F-005).
//!
//! Resolves poster / backdrop / logo / clearart / summary for a title, walking
//! a per-tier per-provider cascade. The cascade is locked by the PRD; only
//! the language chain (tiers 2..=4) is user-configurable through F-016.
//!
//! ## Locked cascade (PRD §F-005)
//!
//! For each image type independently, walk the tiers in order:
//!
//! | Tier | Language        | Provider order             |
//! |------|-----------------|----------------------------|
//! | 1    | primary         | Fanart.tv → TMDB → TVDB    |
//! | 2    | fallback 1      | Fanart.tv → TMDB → TVDB    |
//! | 3    | fallback 2      | Fanart.tv → TMDB → TVDB    |
//! | 4    | fallback 3      | Fanart.tv → TMDB → TVDB    |
//! | 5    | any other lang  | Fanart.tv → TMDB → TVDB    |
//! | 6    | none            | placeholder                |
//!
//! The first non-empty asset returned wins per image type. A provider with no
//! configured API key is skipped (treated as "no asset"). Summary follows the
//! same tier structure but only TMDB and TVDB serve summaries (Fanart.tv is
//! image-only); tier 6 yields an empty string.
//!
//! The cascade itself is pure: it operates on already-fetched
//! [`ProviderBundle`]s and never issues network calls. The host wraps it with
//! [`response_cache`](kino_core::Db) plumbing (`ARTWORK_TTL_S = 7d`) and the
//! cross-provider id resolution required to populate the bundles.

use std::collections::HashMap;
use std::fmt::Write;

use kino_core::title::{Artwork, ImageType, Provenance, TitleKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Provider identifier used in [`Provenance::source`] strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Fanart,
    Tmdb,
    Tvdb,
}

impl Provider {
    /// Short slug embedded in source markers (`fanart.tv:<lang>`, `tmdb:<lang>`,
    /// `tvdb:<lang>`).
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Fanart => "fanart.tv",
            Self::Tmdb => "tmdb",
            Self::Tvdb => "tvdb",
        }
    }
}

/// A single image asset returned by a provider, with the language it's tagged
/// with (the empty string means "no language" / textless artwork, which is
/// always a valid match for every lang tier).
///
/// `Serialize` / `Deserialize` are derived so [`ProviderBundle`] can be
/// persisted in `response_cache` for the per-resource `ETag` round-trip
/// (PRD §F-003; Session 032).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedAsset {
    /// Language tag the provider tagged the asset with. Lowercased BCP-47-ish;
    /// providers normalize ahead of inserting.
    pub lang: String,
    /// Absolute URL to the asset.
    pub url: String,
}

/// All assets returned by a single provider for a single title. Built once
/// per `resolve_artwork` call per provider (zero network if the provider lacks
/// a key) and consumed by the cascade.
///
/// `Serialize` / `Deserialize` are derived so the value can be persisted in
/// `response_cache` for the per-resource `ETag` round-trip
/// (PRD §F-003; Session 032 — TVDB extended-title call site).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderBundle {
    pub posters: Vec<LocalizedAsset>,
    pub backdrops: Vec<LocalizedAsset>,
    pub logos: Vec<LocalizedAsset>,
    pub clearart: Vec<LocalizedAsset>,
    /// `lang -> summary text`. TMDB serves per-language overviews; TVDB serves
    /// `overviewTranslations`. Fanart.tv leaves this empty.
    pub summaries: HashMap<String, String>,
}

/// All bundles consumed by the cascade. A `None` bundle means the provider
/// was skipped (no API key, or no resolvable id of the provider's expected
/// shape).
#[derive(Debug, Default)]
pub struct ProviderBundles {
    pub fanart: Option<ProviderBundle>,
    pub tmdb: Option<ProviderBundle>,
    pub tvdb: Option<ProviderBundle>,
}

/// PRD §F-005 tier-6 placeholder source marker. The frontend resolves this
/// to a bundled SVG asset (the F-008 home screen lands those); the resolver
/// only emits the sentinel URL so the renderer can spot it.
pub const PLACEHOLDER_URL_POSTER: &str = "kino://placeholder/poster.svg";
pub const PLACEHOLDER_URL_BACKDROP: &str = "kino://placeholder/backdrop.svg";
pub const PLACEHOLDER_URL_LOGO: &str = "kino://placeholder/logo.svg";
pub const PLACEHOLDER_URL_CLEARART: &str = "kino://placeholder/clearart.svg";
pub const PLACEHOLDER_SOURCE: &str = "placeholder";

/// Hash the lang chain into a stable hex slug for the cache key. PRD §F-005
/// "Changing the user's language preferences invalidates the cache on next
/// read" is honored by including this hash in the cache key.
#[must_use]
pub fn lang_chain_hash(lang_pref: &[String]) -> String {
    let mut hasher = Sha256::new();
    for (i, lang) in lang_pref.iter().enumerate() {
        if i > 0 {
            hasher.update(b"|");
        }
        hasher.update(lang.as_bytes());
    }
    let digest = hasher.finalize();
    // 16 hex chars (64 bits) is enough to make collisions negligible at the
    // per-title granularity of this cache.
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Resolve the [`Artwork`] for a title from the provided bundles.
///
/// Pure: no network, no I/O. The caller (the Tauri host) builds the bundles
/// from the per-provider HTTP clients and feeds them in; the cascade itself
/// is deterministic.
#[must_use]
pub fn cascade(_kind: TitleKind, bundles: &ProviderBundles, lang_pref: &[String]) -> Artwork {
    let poster = pick_image(bundles, ImageType::Poster, lang_pref);
    let backdrop = pick_image(bundles, ImageType::Backdrop, lang_pref);
    let logo = pick_image(bundles, ImageType::Logo, lang_pref);
    let clearart = pick_image(bundles, ImageType::Clearart, lang_pref);
    let summary = pick_summary(bundles, lang_pref);
    Artwork {
        poster: poster.url,
        backdrop: backdrop.url,
        logo: logo.url,
        clearart: clearart.url,
        summary: summary.text,
        sources: Provenance {
            poster: poster.source,
            backdrop: backdrop.source,
            logo: logo.source,
            clearart: clearart.source,
            summary: summary.source,
        },
    }
}

/// Result of resolving one image type. Always non-empty: if every tier and
/// every provider yielded nothing, the placeholder URL wins.
struct PickedImage {
    url: String,
    source: String,
}

/// Result of resolving the summary. Empty string + `"placeholder"` source if
/// no provider served a summary (Fanart.tv is summary-blind so this is
/// expected for many titles).
struct PickedSummary {
    text: String,
    source: String,
}

fn pick_image(bundles: &ProviderBundles, ty: ImageType, lang_pref: &[String]) -> PickedImage {
    // Tiers 1..=4: configured languages in order.
    for lang in lang_pref {
        if let Some(picked) = try_lang_image(bundles, ty, lang) {
            return picked;
        }
    }
    // Tier 5: any other language. Each provider in order surrenders whatever
    // it has — the first non-empty asset wins.
    for provider in [Provider::Fanart, Provider::Tmdb, Provider::Tvdb] {
        if let Some(bundle) = bundle_for(bundles, provider) {
            if let Some(asset) = assets_for(bundle, ty).first() {
                return PickedImage {
                    url: asset.url.clone(),
                    source: format!("{}:{}", provider.slug(), source_lang(&asset.lang)),
                };
            }
        }
    }
    // Tier 6: placeholder.
    PickedImage {
        url: placeholder_url(ty).to_string(),
        source: PLACEHOLDER_SOURCE.to_string(),
    }
}

fn try_lang_image(bundles: &ProviderBundles, ty: ImageType, lang: &str) -> Option<PickedImage> {
    let normalized = normalize_lang(lang);
    for provider in [Provider::Fanart, Provider::Tmdb, Provider::Tvdb] {
        let Some(bundle) = bundle_for(bundles, provider) else {
            continue;
        };
        let assets = assets_for(bundle, ty);
        if let Some(asset) = assets.iter().find(|a| lang_matches(&a.lang, &normalized)) {
            return Some(PickedImage {
                url: asset.url.clone(),
                source: format!("{}:{}", provider.slug(), lang),
            });
        }
    }
    None
}

fn pick_summary(bundles: &ProviderBundles, lang_pref: &[String]) -> PickedSummary {
    // Tiers 1..=4. Fanart never serves summaries, so the order is TMDB → TVDB.
    for lang in lang_pref {
        let normalized = normalize_lang(lang);
        for provider in [Provider::Tmdb, Provider::Tvdb] {
            if let Some(bundle) = bundle_for(bundles, provider) {
                if let Some(text) = lookup_summary(&bundle.summaries, &normalized) {
                    if !text.is_empty() {
                        return PickedSummary {
                            text: text.clone(),
                            source: format!("{}:{}", provider.slug(), lang),
                        };
                    }
                }
            }
        }
    }
    // Tier 5: any other language. Sort the available langs deterministically
    // so consecutive calls return the same picked summary.
    for provider in [Provider::Tmdb, Provider::Tvdb] {
        if let Some(bundle) = bundle_for(bundles, provider) {
            let mut entries: Vec<(&String, &String)> = bundle
                .summaries
                .iter()
                .filter(|(_, text)| !text.is_empty())
                .collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            if let Some((lang, text)) = entries.first() {
                return PickedSummary {
                    text: (*text).clone(),
                    source: format!("{}:{}", provider.slug(), source_lang(lang)),
                };
            }
        }
    }
    // Tier 6: empty.
    PickedSummary {
        text: String::new(),
        source: PLACEHOLDER_SOURCE.to_string(),
    }
}

fn assets_for(bundle: &ProviderBundle, ty: ImageType) -> &Vec<LocalizedAsset> {
    match ty {
        ImageType::Poster => &bundle.posters,
        ImageType::Backdrop => &bundle.backdrops,
        ImageType::Logo => &bundle.logos,
        ImageType::Clearart => &bundle.clearart,
    }
}

fn bundle_for(bundles: &ProviderBundles, provider: Provider) -> Option<&ProviderBundle> {
    match provider {
        Provider::Fanart => bundles.fanart.as_ref(),
        Provider::Tmdb => bundles.tmdb.as_ref(),
        Provider::Tvdb => bundles.tvdb.as_ref(),
    }
}

fn placeholder_url(ty: ImageType) -> &'static str {
    match ty {
        ImageType::Poster => PLACEHOLDER_URL_POSTER,
        ImageType::Backdrop => PLACEHOLDER_URL_BACKDROP,
        ImageType::Logo => PLACEHOLDER_URL_LOGO,
        ImageType::Clearart => PLACEHOLDER_URL_CLEARART,
    }
}

/// An asset's lang matches the requested lang if they normalize to the same
/// 2-letter prefix OR if the asset has no language tag (textless artwork is
/// universally appropriate for any language tier).
fn lang_matches(asset_lang: &str, requested: &str) -> bool {
    if asset_lang.is_empty() {
        return true;
    }
    let a = normalize_lang(asset_lang);
    a == requested
}

fn lookup_summary<'a>(map: &'a HashMap<String, String>, requested: &str) -> Option<&'a String> {
    if let Some(text) = map.get(requested) {
        return Some(text);
    }
    // Fall through to prefix match: providers sometimes key summaries under
    // a 3-letter ISO 639-2 code (`eng`) while the requested lang is 2-letter
    // (`en`). The fanout below covers both ways.
    map.iter()
        .find(|(k, _)| normalize_lang(k) == requested)
        .map(|(_, v)| v)
}

/// Normalize a lang tag into a stable 2-letter lower-case prefix used for
/// matching. Accepts `"en"`, `"en-US"`, `"eng"` (TVDB) all of which collapse
/// to `"en"`. Empty input stays empty.
fn normalize_lang(lang: &str) -> String {
    let trimmed = lang.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return String::new();
    }
    // BCP-47-ish: first subtag, truncated to 2 chars. ISO 639-2 codes
    // (`eng`, `fre`, `spa`) map to their canonical 2-letter prefix.
    let primary = trimmed.split(['-', '_']).next().unwrap_or("");
    match primary {
        "eng" => "en".to_string(),
        "fre" | "fra" => "fr".to_string(),
        "spa" => "es".to_string(),
        "ger" | "deu" => "de".to_string(),
        "ita" => "it".to_string(),
        "jpn" => "ja".to_string(),
        "rus" => "ru".to_string(),
        "por" => "pt".to_string(),
        "chi" | "zho" => "zh".to_string(),
        "kor" => "ko".to_string(),
        "ara" => "ar".to_string(),
        "nld" | "dut" => "nl".to_string(),
        other => {
            if other.len() >= 2 {
                other.chars().take(2).collect()
            } else {
                other.to_string()
            }
        }
    }
}

/// Render a per-asset lang into the `<provider>:<lang>` source marker. Empty
/// lang renders as `"none"` (textless artwork has no language).
fn source_lang(lang: &str) -> String {
    if lang.is_empty() {
        "none".to_string()
    } else {
        lang.to_string()
    }
}

/// Tag-along DTO for serializing the cache row. The wire payload is
/// `{"artwork": Artwork}`; wrapping makes future schema growth painless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedArtwork {
    pub artwork: Artwork,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(lang: &str, url: &str) -> LocalizedAsset {
        LocalizedAsset {
            lang: lang.to_string(),
            url: url.to_string(),
        }
    }

    fn fanart_bundle() -> ProviderBundle {
        ProviderBundle {
            posters: vec![asset("en", "https://fanart/en/poster.png")],
            backdrops: vec![asset("", "https://fanart/textless/back.png")],
            logos: vec![asset("fr", "https://fanart/fr/logo.png")],
            clearart: vec![asset("en", "https://fanart/en/clearart.png")],
            summaries: HashMap::new(),
        }
    }

    fn tmdb_bundle() -> ProviderBundle {
        let mut summaries = HashMap::new();
        summaries.insert("en".to_string(), "English overview".to_string());
        summaries.insert("fr".to_string(), "Description française".to_string());
        ProviderBundle {
            posters: vec![asset("fr", "https://tmdb/fr/poster.png")],
            backdrops: vec![asset("en", "https://tmdb/en/back.png")],
            logos: vec![asset("en", "https://tmdb/en/logo.png")],
            clearart: vec![],
            summaries,
        }
    }

    fn tvdb_bundle() -> ProviderBundle {
        let mut summaries = HashMap::new();
        summaries.insert("eng".to_string(), "TVDB English".to_string());
        ProviderBundle {
            posters: vec![asset("en", "https://tvdb/en/poster.png")],
            backdrops: vec![asset("en", "https://tvdb/en/back.png")],
            logos: vec![],
            clearart: vec![asset("eng", "https://tvdb/en/clearart.png")],
            summaries,
        }
    }

    #[test]
    fn tier1_fanart_wins_when_present() {
        let bundles = ProviderBundles {
            fanart: Some(fanart_bundle()),
            tmdb: Some(tmdb_bundle()),
            tvdb: Some(tvdb_bundle()),
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.poster, "https://fanart/en/poster.png");
        assert_eq!(art.sources.poster, "fanart.tv:en");
    }

    #[test]
    fn tier1_falls_to_tmdb_when_fanart_missing_lang() {
        // No fanart bundle: TMDB tier 1 wins.
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(tmdb_bundle()),
            tvdb: Some(tvdb_bundle()),
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.backdrop, "https://tmdb/en/back.png");
        assert_eq!(art.sources.backdrop, "tmdb:en");
    }

    #[test]
    fn tier1_falls_to_tvdb_when_others_lack_asset() {
        // Fanart has no logo for "es"; TMDB has no logo for "es"; TVDB
        // doesn't either. But for poster, only TVDB has "en" if both
        // higher providers happen to lack it.
        let bundles = ProviderBundles {
            fanart: Some(ProviderBundle {
                posters: vec![asset("de", "https://fanart/de/poster.png")],
                ..ProviderBundle::default()
            }),
            tmdb: Some(ProviderBundle {
                posters: vec![asset("fr", "https://tmdb/fr/poster.png")],
                ..ProviderBundle::default()
            }),
            tvdb: Some(ProviderBundle {
                posters: vec![asset("eng", "https://tvdb/en/poster.png")],
                ..ProviderBundle::default()
            }),
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.poster, "https://tvdb/en/poster.png");
        assert_eq!(art.sources.poster, "tvdb:en");
    }

    #[test]
    fn tier2_fallback_lang_resolves_when_tier1_empty() {
        // No "en" assets anywhere; "fr" exists in TMDB.
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(ProviderBundle {
                posters: vec![asset("fr", "https://tmdb/fr/poster.png")],
                ..ProviderBundle::default()
            }),
            tvdb: None,
        };
        let art = cascade(
            TitleKind::Movie,
            &bundles,
            &["en".to_string(), "fr".to_string()],
        );
        assert_eq!(art.poster, "https://tmdb/fr/poster.png");
        assert_eq!(art.sources.poster, "tmdb:fr");
    }

    #[test]
    fn tier5_any_language_picks_first_available() {
        // No matches in configured chain ["en", "fr"]; only "ja" exists.
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(ProviderBundle {
                posters: vec![asset("ja", "https://tmdb/ja/poster.png")],
                ..ProviderBundle::default()
            }),
            tvdb: None,
        };
        let art = cascade(
            TitleKind::Movie,
            &bundles,
            &["en".to_string(), "fr".to_string()],
        );
        assert_eq!(art.poster, "https://tmdb/ja/poster.png");
        assert_eq!(art.sources.poster, "tmdb:ja");
    }

    #[test]
    fn tier6_placeholder_when_no_provider_has_asset() {
        let bundles = ProviderBundles::default();
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.poster, PLACEHOLDER_URL_POSTER);
        assert_eq!(art.sources.poster, PLACEHOLDER_SOURCE);
        assert_eq!(art.backdrop, PLACEHOLDER_URL_BACKDROP);
        assert_eq!(art.logo, PLACEHOLDER_URL_LOGO);
        assert_eq!(art.clearart, PLACEHOLDER_URL_CLEARART);
    }

    #[test]
    fn missing_fanart_key_still_resolves_via_tmdb_tvdb() {
        // PRD §F-005 acceptance: a title with missing Fanart.tv key still
        // resolves via TMDB/TVDB across all language tiers.
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(tmdb_bundle()),
            tvdb: Some(tvdb_bundle()),
        };
        let art = cascade(
            TitleKind::Movie,
            &bundles,
            &["en".to_string(), "fr".to_string()],
        );
        // Poster: TMDB has fr, TVDB has en. Tier 1 (en): TMDB has no en,
        // TVDB has en → wins.
        assert_eq!(art.poster, "https://tvdb/en/poster.png");
        assert_eq!(art.sources.poster, "tvdb:en");
    }

    #[test]
    fn per_image_type_independence_demonstrated() {
        // PRD §F-005: "It is normal for a title to end up with poster from
        // Fanart.tv tier 1, backdrop from TMDB tier 2, no logo, and clearart
        // from placeholder."
        let bundles = ProviderBundles {
            fanart: Some(ProviderBundle {
                posters: vec![asset("en", "https://fanart/en/poster.png")],
                ..ProviderBundle::default()
            }),
            tmdb: Some(ProviderBundle {
                backdrops: vec![asset("fr", "https://tmdb/fr/back.png")],
                ..ProviderBundle::default()
            }),
            tvdb: None,
        };
        let art = cascade(
            TitleKind::Movie,
            &bundles,
            &["en".to_string(), "fr".to_string()],
        );
        assert_eq!(art.poster, "https://fanart/en/poster.png");
        assert_eq!(art.sources.poster, "fanart.tv:en");
        assert_eq!(art.backdrop, "https://tmdb/fr/back.png");
        assert_eq!(art.sources.backdrop, "tmdb:fr");
        assert_eq!(art.logo, PLACEHOLDER_URL_LOGO);
        assert_eq!(art.sources.logo, PLACEHOLDER_SOURCE);
        assert_eq!(art.clearart, PLACEHOLDER_URL_CLEARART);
    }

    #[test]
    fn summary_skips_fanart_uses_tmdb_then_tvdb() {
        // PRD §F-005: Summary uses TMDB → TVDB; Fanart never serves summary
        // even if its bundle is present.
        let bundles = ProviderBundles {
            fanart: Some(fanart_bundle()),
            tmdb: None,
            tvdb: Some(tvdb_bundle()),
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.summary, "TVDB English");
        assert_eq!(art.sources.summary, "tvdb:en");
    }

    #[test]
    fn summary_prefers_tmdb_over_tvdb_within_a_tier() {
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(tmdb_bundle()),
            tvdb: Some(tvdb_bundle()),
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.summary, "English overview");
        assert_eq!(art.sources.summary, "tmdb:en");
    }

    #[test]
    fn summary_falls_to_fallback_lang_when_primary_missing() {
        let mut tmdb = tmdb_bundle();
        tmdb.summaries.remove("en");
        let bundles = ProviderBundles {
            fanart: None,
            tmdb: Some(tmdb),
            tvdb: None,
        };
        let art = cascade(
            TitleKind::Movie,
            &bundles,
            &["en".to_string(), "fr".to_string()],
        );
        assert_eq!(art.summary, "Description française");
        assert_eq!(art.sources.summary, "tmdb:fr");
    }

    #[test]
    fn summary_tier6_empty_when_no_provider_serves_one() {
        let bundles = ProviderBundles {
            fanart: Some(fanart_bundle()),
            tmdb: None,
            tvdb: None,
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.summary, "");
        assert_eq!(art.sources.summary, PLACEHOLDER_SOURCE);
    }

    #[test]
    fn textless_artwork_matches_any_language() {
        // An asset tagged with empty lang (textless logo / backdrop) is
        // universally appropriate. PRD §F-005 doesn't require this but it's
        // a robustness improvement that prevents "no logo" results when the
        // upstream only has a textless logo.
        let bundles = ProviderBundles {
            fanart: Some(ProviderBundle {
                logos: vec![asset("", "https://fanart/textless/logo.png")],
                ..ProviderBundle::default()
            }),
            tmdb: None,
            tvdb: None,
        };
        let art = cascade(TitleKind::Movie, &bundles, &["en".to_string()]);
        assert_eq!(art.logo, "https://fanart/textless/logo.png");
        assert_eq!(art.sources.logo, "fanart.tv:en");
    }

    #[test]
    fn lang_chain_hash_is_stable_and_chain_sensitive() {
        let a = lang_chain_hash(&["en".to_string(), "fr".to_string()]);
        let b = lang_chain_hash(&["en".to_string(), "fr".to_string()]);
        assert_eq!(a, b);
        let c = lang_chain_hash(&["fr".to_string(), "en".to_string()]);
        assert_ne!(a, c);
        let d = lang_chain_hash(&["en".to_string()]);
        assert_ne!(a, d);
    }

    #[test]
    fn normalize_lang_collapses_iso_639_2_to_two_letter() {
        assert_eq!(normalize_lang("en"), "en");
        assert_eq!(normalize_lang("en-US"), "en");
        assert_eq!(normalize_lang("eng"), "en");
        assert_eq!(normalize_lang("fre"), "fr");
        assert_eq!(normalize_lang("fra"), "fr");
        assert_eq!(normalize_lang(""), "");
    }
}
