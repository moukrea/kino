//! Stremio addon manifest parsing & validation (PRD §F-007).
//!
//! Per the PRD, manifest validation checks the presence of `id`, `version`,
//! `name`, `types`, `resources`, and `catalogs`. Other fields (description,
//! logo, background, idPrefixes, behaviorHints, etc.) are tolerated and
//! preserved in the raw JSON the persistence layer stores. Stremio's spec is
//! tolerant of additional fields; we mirror that.
//!
//! The validator returns a typed [`ManifestError`] enum so the
//! `install_addon` command can surface a precise message to the UI — F-016
//! "Setup wizard" uses this to tell the user why an addon URL didn't work.

use serde::{Deserialize, Serialize};

/// A parsed and validated Stremio addon manifest.
///
/// Mirrors the fields PRD §F-007 requires; the full raw JSON is also kept
/// so the persistence layer can store the verbatim blob alongside the
/// extracted summary (matches `kino_core::addon::Addon::manifest_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable addon identifier. The protocol layer trusts this; the install
    /// flow uses it as the persisted `addons.id` primary key.
    pub id: String,
    /// `semver`-ish version string. Stremio does not enforce semver; we
    /// store it verbatim.
    pub version: String,
    /// Human-readable addon name.
    pub name: String,
    /// Optional one-line description.
    #[serde(default)]
    pub description: Option<String>,
    /// Title kinds this addon serves catalogs for (e.g. `["movie", "series"]`).
    pub types: Vec<String>,
    /// Addon protocol resources this addon serves
    /// (e.g. `["catalog", "meta", "stream", "subtitles"]`).
    pub resources: Vec<ManifestResource>,
    /// Catalog descriptors. May be empty for stream-only or subtitles-only
    /// addons.
    pub catalogs: Vec<CatalogDescriptor>,
    /// Optional id prefixes the addon claims to handle (e.g. `["tt"]`).
    #[serde(default, rename = "idPrefixes")]
    pub id_prefixes: Vec<String>,
    /// Optional addon-supplied behavior hints (Stremio uses this for
    /// `configurable`, `configurationRequired`, etc.). Stored verbatim.
    #[serde(default, rename = "behaviorHints")]
    pub behavior_hints: serde_json::Value,
}

/// An entry inside the manifest's `resources` array. Stremio accepts both
/// the short form (a bare string like `"catalog"`) and the long form
/// (`{ "name": "stream", "types": [...] }`); we accept both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ManifestResource {
    /// Short form — just the resource name.
    Name(String),
    /// Long form with optional `types` / `idPrefixes` narrowing.
    Detailed {
        name: String,
        #[serde(default)]
        types: Vec<String>,
        #[serde(default, rename = "idPrefixes")]
        id_prefixes: Vec<String>,
    },
}

impl ManifestResource {
    /// Returns the resource name regardless of which serialized form was
    /// used (`"catalog"`, `"meta"`, `"stream"`, `"subtitles"`).
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::Detailed { name, .. } => name,
        }
    }
}

/// Describes one catalog the addon serves. Stremio's protocol locks the
/// `type` / `id` / `name` triple; addons may declare `extra` (search,
/// pagination, filters) which we surface verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogDescriptor {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Errors surfaced by [`parse_manifest`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    /// The response body was not valid JSON.
    #[error("manifest is not valid JSON: {0}")]
    NotJson(String),

    /// JSON shape doesn't match a Stremio manifest (e.g. wrong types,
    /// missing required field, malformed `resources`).
    #[error("malformed manifest: {0}")]
    Malformed(String),

    /// PRD §F-007 required field missing (`id`, `version`, `name`, `types`,
    /// `resources`, `catalogs`).
    #[error("manifest missing required field '{0}'")]
    MissingField(&'static str),

    /// Required field present but empty (e.g. `types: []`, `resources: []`).
    #[error("manifest required field '{0}' is empty")]
    EmptyField(&'static str),
}

/// Parse a manifest JSON body and validate it against PRD §F-007 rules.
///
/// The deserializer is intentionally permissive about additional fields so
/// future Stremio extensions don't break installs; only the locked required
/// fields are policed.
pub fn parse_manifest(body: &str) -> Result<Manifest, ManifestError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| ManifestError::NotJson(e.to_string()))?;
    validate_required_fields(&value)?;
    serde_json::from_value::<Manifest>(value).map_err(|e| ManifestError::Malformed(e.to_string()))
}

fn validate_required_fields(value: &serde_json::Value) -> Result<(), ManifestError> {
    let obj = value
        .as_object()
        .ok_or(ManifestError::Malformed("expected JSON object".into()))?;
    for field in REQUIRED_STRING_FIELDS {
        let entry = obj.get(*field).ok_or(ManifestError::MissingField(field))?;
        let s = entry
            .as_str()
            .ok_or_else(|| ManifestError::Malformed(format!("'{field}' must be a string")))?;
        if s.is_empty() {
            return Err(ManifestError::EmptyField(field));
        }
    }
    for field in REQUIRED_ARRAY_FIELDS {
        let entry = obj.get(*field).ok_or(ManifestError::MissingField(field))?;
        let arr = entry
            .as_array()
            .ok_or_else(|| ManifestError::Malformed(format!("'{field}' must be an array")))?;
        // PRD §F-007 wording is "presence of" the field; an empty `catalogs`
        // array is legal (stream-only / subtitles-only addons declare an
        // empty catalogs list). `types` and `resources` SHOULD be non-empty
        // — an addon that serves no title kinds and no protocol resources
        // is functionally a no-op. Mirror Stremio's behavior: reject empty
        // `types` / `resources`, allow empty `catalogs`.
        if arr.is_empty() && *field != "catalogs" {
            return Err(ManifestError::EmptyField(field));
        }
    }
    Ok(())
}

const REQUIRED_STRING_FIELDS: &[&str] = &["id", "version", "name"];
const REQUIRED_ARRAY_FIELDS: &[&str] = &["types", "resources", "catalogs"];

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest_json() -> &'static str {
        r#"{
            "id": "com.example.addon",
            "version": "1.0.0",
            "name": "Example Addon",
            "description": "Example",
            "types": ["movie", "series"],
            "resources": ["catalog", "meta"],
            "catalogs": [
                {"type": "movie", "id": "top", "name": "Top Movies"}
            ]
        }"#
    }

    #[test]
    fn parses_valid_manifest() {
        let m = parse_manifest(valid_manifest_json()).unwrap();
        assert_eq!(m.id, "com.example.addon");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.name, "Example Addon");
        assert_eq!(m.types, vec!["movie", "series"]);
        assert_eq!(m.resources.len(), 2);
        assert_eq!(m.resources[0].name(), "catalog");
        assert_eq!(m.catalogs.len(), 1);
        assert_eq!(m.catalogs[0].kind, "movie");
        assert_eq!(m.catalogs[0].id, "top");
    }

    #[test]
    fn accepts_long_form_resources() {
        let body = r#"{
            "id": "com.example.addon",
            "version": "1.0.0",
            "name": "Example",
            "types": ["movie"],
            "resources": [
                {"name": "stream", "types": ["movie"], "idPrefixes": ["tt"]}
            ],
            "catalogs": []
        }"#;
        let m = parse_manifest(body).unwrap();
        assert_eq!(m.resources[0].name(), "stream");
    }

    #[test]
    fn accepts_empty_catalogs_for_stream_only_addons() {
        let body = r#"{
            "id": "com.example.stream-only",
            "version": "1.0.0",
            "name": "Stream Only",
            "types": ["movie"],
            "resources": ["stream"],
            "catalogs": []
        }"#;
        assert!(parse_manifest(body).is_ok());
    }

    #[test]
    fn rejects_missing_id() {
        let body = r#"{
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        let err = parse_manifest(body).unwrap_err();
        assert!(matches!(err, ManifestError::MissingField("id")));
    }

    #[test]
    fn rejects_missing_version() {
        let body = r#"{
            "id": "x",
            "name": "X",
            "types": ["movie"],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::MissingField("version")
        );
    }

    #[test]
    fn rejects_missing_name() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "types": ["movie"],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::MissingField("name")
        );
    }

    #[test]
    fn rejects_missing_types() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "name": "X",
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::MissingField("types")
        );
    }

    #[test]
    fn rejects_missing_resources() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::MissingField("resources")
        );
    }

    #[test]
    fn rejects_missing_catalogs() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "resources": ["catalog"]
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::MissingField("catalogs")
        );
    }

    #[test]
    fn rejects_empty_types() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "name": "X",
            "types": [],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::EmptyField("types")
        );
    }

    #[test]
    fn rejects_empty_resources() {
        let body = r#"{
            "id": "x",
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "resources": [],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::EmptyField("resources")
        );
    }

    #[test]
    fn rejects_empty_string_id() {
        let body = r#"{
            "id": "",
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        assert_eq!(
            parse_manifest(body).unwrap_err(),
            ManifestError::EmptyField("id")
        );
    }

    #[test]
    fn rejects_invalid_json() {
        let err = parse_manifest("not json").unwrap_err();
        assert!(matches!(err, ManifestError::NotJson(_)));
    }

    #[test]
    fn rejects_non_object_root() {
        let err = parse_manifest("[]").unwrap_err();
        assert!(matches!(err, ManifestError::Malformed(_)));
    }

    #[test]
    fn rejects_wrong_field_type() {
        let body = r#"{
            "id": 123,
            "version": "1.0.0",
            "name": "X",
            "types": ["movie"],
            "resources": ["catalog"],
            "catalogs": []
        }"#;
        let err = parse_manifest(body).unwrap_err();
        assert!(matches!(err, ManifestError::Malformed(_)));
    }
}
