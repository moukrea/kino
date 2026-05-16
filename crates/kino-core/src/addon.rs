//! Stremio addon catalog entries.
//!
//! Mirrors the `addons` schema in `migrations/0001_init.sql` (PRD §F-002).
//! The full manifest blob is stored verbatim as JSON so the addon-protocol
//! client (PRD §F-007) doesn't have to re-fetch it after installation.

use serde::{Deserialize, Serialize};

/// An installed Stremio addon as stored in the `addons` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Addon {
    /// Stable identifier, typically the manifest's `id` field.
    pub id: String,
    /// Absolute URL of the manifest, e.g. `https://.../manifest.json`.
    pub manifest_url: String,
    /// Whether the addon participates in catalog / stream queries.
    pub enabled: bool,
    /// Unix epoch seconds when the addon was installed.
    pub installed_at: i64,
    /// The verbatim manifest JSON.
    pub manifest_json: serde_json::Value,
    /// User-controlled ordering. Lower sorts first.
    pub display_order: i64,
}

/// Insert shape for [`Addon`]. `installed_at` is set by the persistence layer
/// to `now()`; `enabled` defaults to `true`; `display_order` defaults to the
/// next available slot when omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddonInsert {
    pub id: String,
    pub manifest_url: String,
    pub manifest_json: serde_json::Value,
    #[serde(default)]
    pub display_order: Option<i64>,
}
