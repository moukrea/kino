//! Recommended-addons table. Locked by PRD §8.
//!
//! The Settings → Addons screen offers one-tap install for each entry. The
//! Cinemeta row in this list is also the addon auto-installed on first
//! launch (F-007, ADR-014); it is non-removable in v1.

/// A recommended addon shown in the Settings → Addons one-tap install list.
#[derive(Debug, Clone, Copy)]
pub struct RecommendedAddon {
    pub name: &'static str,
    pub manifest_url: &'static str,
    pub description: &'static str,
}

/// Cinemeta is special-cased: it ships pre-installed and cannot be deleted
/// (only disabled). The F-007 implementation must enforce this — looking up
/// this constant by URL is the canonical check.
pub const CINEMETA_MANIFEST_URL: &str = "https://v3-cinemeta.strem.io/manifest.json";

/// PRD §8 recommended-addons table, in spec order.
pub const RECOMMENDED_ADDONS: &[RecommendedAddon] = &[
    RecommendedAddon {
        name: "Cinemeta",
        manifest_url: CINEMETA_MANIFEST_URL,
        description: "Official metadata catalogs (pre-installed)",
    },
    RecommendedAddon {
        name: "Torrentio",
        manifest_url: "https://torrentio.strem.fun/manifest.json",
        description: "Torrent streams aggregator",
    },
    RecommendedAddon {
        name: "OpenSubtitles v3",
        manifest_url: "https://opensubtitles-v3.strem.io/manifest.json",
        description: "Community subtitles",
    },
    RecommendedAddon {
        name: "Public Domain Movies",
        manifest_url: "https://public-domain-movies.now.sh/manifest.json",
        description: "Free public domain titles",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_prd() {
        assert_eq!(RECOMMENDED_ADDONS.len(), 4);
    }

    #[test]
    fn cinemeta_is_first_and_pinned() {
        let first = &RECOMMENDED_ADDONS[0];
        assert_eq!(first.name, "Cinemeta");
        assert_eq!(first.manifest_url, CINEMETA_MANIFEST_URL);
    }

    #[test]
    fn all_urls_are_https_manifests() {
        for a in RECOMMENDED_ADDONS {
            assert!(
                a.manifest_url.starts_with("https://"),
                "{} is not https",
                a.name
            );
            assert!(
                a.manifest_url.ends_with("/manifest.json"),
                "{} does not end with /manifest.json",
                a.name
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = RECOMMENDED_ADDONS.iter().map(|a| a.name).collect();
        names.sort_unstable();
        let len = names.len();
        names.dedup();
        assert_eq!(names.len(), len);
    }
}
