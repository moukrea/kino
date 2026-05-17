//! URL normalization for Stremio addon manifest URLs (PRD §F-007).
//!
//! Stremio advertises addons with two equivalent URL schemes:
//!
//! - `https://<host>/<path>/manifest.json` — directly fetchable.
//! - `stremio://<host>/<path>/manifest.json` — a Stremio-specific scheme
//!   that historically pasted into desktop Stremio's "install addon" field.
//!
//! Per PRD §F-007 the addon client accepts both, normalizing
//! `stremio://...` to `https://...` before any HTTP call. The
//! [`normalize_manifest_url`] helper performs that conversion and also
//! enforces the `/manifest.json` suffix shape so install flows can reject
//! obviously-wrong inputs before issuing a fetch.

use crate::AddonError;

/// Normalize a user-supplied addon manifest URL.
///
/// Returns the canonical `https://...manifest.json` form. Accepts
/// `https://`, `http://` (preserved verbatim — public Stremio addons are
/// rare on plain HTTP but we don't gate on TLS at this layer), and
/// `stremio://` (rewritten to `https://`). Other schemes — or a URL whose
/// path does not end in `manifest.json` — return [`AddonError::InvalidUrl`].
pub fn normalize_manifest_url(url: &str) -> Result<String, AddonError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(AddonError::InvalidUrl("empty url".into()));
    }
    let normalized = if let Some(rest) = trimmed.strip_prefix("stremio://") {
        format!("https://{rest}")
    } else if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_string()
    } else {
        return Err(AddonError::InvalidUrl(format!(
            "unsupported scheme in '{trimmed}': expected https://, http://, or stremio://"
        )));
    };
    if !normalized.ends_with("/manifest.json") {
        return Err(AddonError::InvalidUrl(format!(
            "expected URL ending in '/manifest.json', got '{normalized}'"
        )));
    }
    Ok(normalized)
}

/// Strip the trailing `/manifest.json` from a normalized manifest URL,
/// returning the addon base URL the protocol client uses for catalog /
/// meta / stream calls.
pub fn base_url_from_manifest(manifest_url: &str) -> &str {
    manifest_url
        .strip_suffix("/manifest.json")
        .unwrap_or(manifest_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_passes_through_unchanged() {
        let out = normalize_manifest_url("https://v3-cinemeta.strem.io/manifest.json").unwrap();
        assert_eq!(out, "https://v3-cinemeta.strem.io/manifest.json");
    }

    #[test]
    fn stremio_scheme_rewrites_to_https() {
        let out = normalize_manifest_url("stremio://v3-cinemeta.strem.io/manifest.json").unwrap();
        assert_eq!(out, "https://v3-cinemeta.strem.io/manifest.json");
    }

    #[test]
    fn http_passes_through_unchanged() {
        let out = normalize_manifest_url("http://localhost:7000/manifest.json").unwrap();
        assert_eq!(out, "http://localhost:7000/manifest.json");
    }

    #[test]
    fn whitespace_is_trimmed() {
        let out = normalize_manifest_url("  https://example.com/manifest.json  ").unwrap();
        assert_eq!(out, "https://example.com/manifest.json");
    }

    #[test]
    fn empty_input_is_rejected() {
        let err = normalize_manifest_url("").unwrap_err();
        assert!(matches!(err, AddonError::InvalidUrl(_)));
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let err = normalize_manifest_url("ftp://example.com/manifest.json").unwrap_err();
        assert!(matches!(err, AddonError::InvalidUrl(_)));
    }

    #[test]
    fn missing_manifest_suffix_is_rejected() {
        let err = normalize_manifest_url("https://example.com/").unwrap_err();
        assert!(matches!(err, AddonError::InvalidUrl(_)));
    }

    #[test]
    fn base_url_strips_manifest_json() {
        assert_eq!(
            base_url_from_manifest("https://v3-cinemeta.strem.io/manifest.json"),
            "https://v3-cinemeta.strem.io"
        );
        assert_eq!(
            base_url_from_manifest("https://torrentio.strem.fun/providers=yts/manifest.json"),
            "https://torrentio.strem.fun/providers=yts"
        );
    }
}
