//! `kino-addons` — Stremio addon protocol client.
//!
//! The crate ships:
//!
//! - The PRD §8 locked subsystems first introduced in Session 001:
//!   [`parse`] (stream-filename → tags regex set) and [`recommended`]
//!   (default recommended-addons table).
//! - The Session 008 (F-007) protocol layer: URL normalization
//!   ([`url`]), manifest validation ([`manifest`]), protocol response types
//!   ([`protocol`]), and the per-addon HTTP client ([`AddonClient`]).
//!
//! HTTP retry / timeout / User-Agent are inherited from `kino_core::http`
//! (ADR-055) so addon calls honor the same locked policy as metadata
//! providers.

pub mod client;
pub mod manifest;
pub mod parse;
pub mod protocol;
pub mod recommended;
pub mod url;

pub use client::AddonClient;
pub use manifest::{parse_manifest, CatalogDescriptor, Manifest, ManifestError, ManifestResource};
pub use protocol::{
    CatalogResponse, MetaDetail, MetaPreview, MetaResponse, MetaVideo, Stream, StreamResponse,
    Subtitle, SubtitlesResponse,
};
pub use recommended::{RecommendedAddon, CINEMETA_MANIFEST_URL, RECOMMENDED_ADDONS};
pub use url::{base_url_from_manifest, normalize_manifest_url};

use kino_core::http::HttpError;

/// Crate-level error type for the addon protocol client (PRD §F-007).
///
/// The Tauri host converts this to `String` at the IPC boundary per
/// ADR-039; the typed variants survive intra-crate so the `install_addon`
/// command can surface precise diagnostics to F-016's UI (e.g. "this
/// addon's manifest is missing required field 'catalogs'").
#[derive(Debug, thiserror::Error)]
pub enum AddonError {
    /// User-supplied URL is malformed or uses an unsupported scheme.
    #[error("invalid addon URL: {0}")]
    InvalidUrl(String),

    /// Underlying HTTP transport or non-2xx response.
    #[error(transparent)]
    Http(#[from] HttpError),

    /// Response body did not match the expected protocol shape (catalog,
    /// meta, stream, subtitles).
    #[error("decode error: {0}")]
    Decode(String),

    /// Manifest validation failed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// Attempted to uninstall an addon the user is not allowed to remove
    /// (PRD §F-007: Cinemeta is non-removable in v1).
    #[error("cannot remove '{id}': this addon is required by kino and can only be disabled")]
    NonRemovable { id: String },
}
