//! F-016 Settings — KV-backed user-tunable preferences.
//!
//! All user-tunable values live in the `settings` table (PRD §F-002). This
//! module declares:
//!
//! - the canonical KV key for every PRD §F-016 setting,
//! - the [`SettingsView`] aggregate exposed to the frontend by
//!   [`crate::commands::settings_get_all`],
//! - [`load_view`] which reads the table and folds defaults in for absent
//!   keys, and
//! - [`KNOWN_SETTINGS_KEYS`], the allow-list the
//!   [`crate::commands::settings_reset_defaults`] command uses to wipe
//!   user-set values without disturbing system-internal entries
//!   (`install_id`, `addons.bootstrap_done`).
//!
//! ## Why `String`-typed in the KV layer
//!
//! The `settings` table stores `(key TEXT, value TEXT)` rows. Numeric and
//! boolean values are serialized to their natural string form (`"30"`,
//! `"true"`); JSON values (e.g. the fallback-language list) are stored as
//! the JSON string. The aggregated [`SettingsView`] returned over IPC has
//! typed fields so the frontend doesn't re-parse string-coded values.
//!
//! ## Defaults
//!
//! Defaults come from PRD §8 constants (`CACHE_DEFAULT_LINUX_GIB`,
//! `CACHE_DEFAULT_ANDROID_GIB`, `SAFETY_MARGIN_S`, `PREBUFFER_TARGET_S`,
//! `PIECE_PRIORITY_HIGH_WINDOW_S`, `PIECE_PRIORITY_MED_WINDOW_S`,
//! `RECOMPUTE_INTERVAL_S`) and PRD §F-016 itself for the per-section
//! defaults that aren't in the constants table (e.g. tile size = medium,
//! force HW decoder = on, NSFW = off).

use kino_core::constants::{
    CACHE_DEFAULT_ANDROID_GIB, CACHE_DEFAULT_LINUX_GIB, PIECE_PRIORITY_HIGH_WINDOW_S,
    PIECE_PRIORITY_MED_WINDOW_S, PREBUFFER_TARGET_S, RECOMPUTE_INTERVAL_S, SAFETY_MARGIN_S,
};
use kino_core::Db;
use serde::{Deserialize, Serialize};

use kino_metadata::{FANART_API_KEY, TMDB_API_KEY, TRAKT_API_KEY, TVDB_API_KEY};

// ---- KV keys ----------------------------------------------------------------

// API keys are re-exported from `kino_metadata` (above) so the rest of the
// host code shares one canonical spelling.

/// User-selected primary metadata language (BCP 47, e.g. `"en"`, `"fr-CA"`).
/// Empty/missing → use system default at call sites.
pub const META_PRIMARY_LANG_KEY: &str = "lang.metadata_primary";

/// Fallback metadata language chain, JSON-encoded list (e.g. `["en","fr"]`).
/// PRD §F-016 §3 caps the chain at 3.
pub const META_FALLBACK_LANGS_KEY: &str = "lang.metadata_fallback";

/// UI language (`"en"` or `"fr"`); empty → system default.
pub const UI_LANG_KEY: &str = "lang.ui";

/// Cache directory path. Empty/missing → default under app config dir.
pub const CACHE_PATH_KEY: &str = "cache.path";

/// Cache size limit in GiB. PRD §F-016 §4: 1-100 GiB Linux, 1-50 GiB Android.
pub const CACHE_SIZE_GIB_KEY: &str = "cache.size_gib";

/// F-014 safety margin in seconds (PRD §F-014 default 30).
pub const BUFFER_SAFETY_MARGIN_S_KEY: &str = "buffer.safety_margin_s";

/// F-014 initial prebuffer target in seconds (PRD §F-014 default 15).
pub const BUFFER_PREBUFFER_TARGET_S_KEY: &str = "buffer.prebuffer_target_s";

/// F-014 high-priority piece window in seconds (PRD §8 default 60).
pub const BUFFER_PIECE_HIGH_S_KEY: &str = "buffer.piece_high_s";

/// F-014 medium-priority piece window in seconds (PRD §8 default 300).
pub const BUFFER_PIECE_MED_S_KEY: &str = "buffer.piece_med_s";

/// F-014 recompute interval in seconds (PRD §8 default 5).
pub const BUFFER_RECOMPUTE_INTERVAL_S_KEY: &str = "buffer.recompute_interval_s";

/// PRD §F-016 §6 per-codec passthrough toggles (Android only). Defaults to
/// `true` for every codec — pass through whatever the AVR can decode and let
/// the user disable on a per-codec basis if a chain misbehaves.
pub const PLAYER_PASSTHROUGH_TRUEHD_KEY: &str = "player.passthrough.truehd";
pub const PLAYER_PASSTHROUGH_DTSHD_KEY: &str = "player.passthrough.dtshd";
pub const PLAYER_PASSTHROUGH_DTSX_KEY: &str = "player.passthrough.dtsx";
pub const PLAYER_PASSTHROUGH_ATMOS_KEY: &str = "player.passthrough.atmos";
pub const PLAYER_PASSTHROUGH_AC3_KEY: &str = "player.passthrough.ac3";
pub const PLAYER_PASSTHROUGH_DTS_KEY: &str = "player.passthrough.dts";
pub const PLAYER_PASSTHROUGH_EAC3_KEY: &str = "player.passthrough.eac3";

/// PRD §F-016 §6 force hardware decoder. Default `true`.
pub const PLAYER_FORCE_HW_DECODE_KEY: &str = "player.force_hw_decode";

/// PRD §F-016 §6 tunneling mode (Android TV only). Default `true`.
pub const PLAYER_TUNNELING_KEY: &str = "player.tunneling";

/// PRD §F-016 §7 tile size: `"small" | "medium" | "large"`. Default `"medium"`.
pub const DISPLAY_TILE_SIZE_KEY: &str = "display.tile_size";

/// PRD §F-016 §7 focus animation toggle. Default `true`.
pub const DISPLAY_FOCUS_ANIMATION_KEY: &str = "display.focus_animation";

/// PRD §F-016 §7 show NSFW content (passed to addons). Default `false`.
pub const DISPLAY_NSFW_KEY: &str = "display.nsfw";

/// PRD §F-016 §7 input profile override
/// (`"auto" | "touch" | "dpad" | "kbm"`). Default `"auto"`.
pub const DISPLAY_INPUT_OVERRIDE_KEY: &str = "display.input_override";

/// PRD §F-016 §7 high-contrast theme toggle. Default `false`.
pub const DISPLAY_HIGH_CONTRAST_KEY: &str = "display.high_contrast";

/// PRD §5 Logging "advanced logging" toggle. When `true`, the live
/// `tracing` `EnvFilter` is reloaded to `debug`; when `false` (default),
/// it is reset to `info`. Surfaced in PRD §F-016 §7 Display alongside the
/// other display-level reliability toggles.
pub const DISPLAY_ADVANCED_LOGGING_KEY: &str = "display.advanced_logging";

/// Every PRD §F-016-defined KV key, used by `settings_reset_defaults` to
/// wipe only user-settable values. System-internal entries (`install_id`,
/// `addons.bootstrap_done`) are deliberately NOT listed.
pub const KNOWN_SETTINGS_KEYS: &[&str] = &[
    TMDB_API_KEY,
    TRAKT_API_KEY,
    TVDB_API_KEY,
    FANART_API_KEY,
    META_PRIMARY_LANG_KEY,
    META_FALLBACK_LANGS_KEY,
    UI_LANG_KEY,
    CACHE_PATH_KEY,
    CACHE_SIZE_GIB_KEY,
    BUFFER_SAFETY_MARGIN_S_KEY,
    BUFFER_PREBUFFER_TARGET_S_KEY,
    BUFFER_PIECE_HIGH_S_KEY,
    BUFFER_PIECE_MED_S_KEY,
    BUFFER_RECOMPUTE_INTERVAL_S_KEY,
    PLAYER_PASSTHROUGH_TRUEHD_KEY,
    PLAYER_PASSTHROUGH_DTSHD_KEY,
    PLAYER_PASSTHROUGH_DTSX_KEY,
    PLAYER_PASSTHROUGH_ATMOS_KEY,
    PLAYER_PASSTHROUGH_AC3_KEY,
    PLAYER_PASSTHROUGH_DTS_KEY,
    PLAYER_PASSTHROUGH_EAC3_KEY,
    PLAYER_FORCE_HW_DECODE_KEY,
    PLAYER_TUNNELING_KEY,
    DISPLAY_TILE_SIZE_KEY,
    DISPLAY_FOCUS_ANIMATION_KEY,
    DISPLAY_NSFW_KEY,
    DISPLAY_INPUT_OVERRIDE_KEY,
    DISPLAY_HIGH_CONTRAST_KEY,
    DISPLAY_ADVANCED_LOGGING_KEY,
];

/// Cache size lower bound shared by both platforms (PRD §F-016 §4).
pub const CACHE_SIZE_MIN_GIB: u32 = 1;
/// Linux upper bound (PRD §F-016 §4).
pub const CACHE_SIZE_LINUX_MAX_GIB: u32 = 100;
/// Android upper bound (PRD §F-016 §4).
pub const CACHE_SIZE_ANDROID_MAX_GIB: u32 = 50;

/// PRD §F-016 §3 caps the fallback chain at 3 entries.
pub const META_FALLBACK_MAX: usize = 3;

// ---- view shape -------------------------------------------------------------

/// Aggregate settings snapshot. Returned as JSON over IPC by
/// [`crate::commands::settings_get_all`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    pub api_keys: ApiKeysView,
    pub language: LanguageView,
    pub cache: CacheView,
    pub buffer: BufferView,
    pub player: PlayerView,
    pub display: DisplayView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApiKeysView {
    pub tmdb: String,
    pub trakt: String,
    pub tvdb: String,
    pub fanart: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LanguageView {
    pub metadata_primary: String,
    pub metadata_fallback: Vec<String>,
    pub ui: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheView {
    pub path: String,
    pub size_gib: u32,
    pub min_gib: u32,
    pub max_gib: u32,
}

// PRD §F-014 buffer values are all in seconds; the shared `_s` suffix
// matches the field names in `kino_core::constants` (`SAFETY_MARGIN_S`
// etc.) and the PRD's own naming, so we keep the suffix for grep-ability.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BufferView {
    pub safety_margin_s: f64,
    pub prebuffer_target_s: f64,
    pub piece_high_s: f64,
    pub piece_med_s: f64,
    pub recompute_interval_s: f64,
}

impl Eq for BufferView {}

// PRD §F-016 §6 enumerates one passthrough toggle per codec plus the two
// platform-level toggles (force hardware decoder, tunneling). The high
// boolean count IS the spec; a state-machine refactor would change the
// shape of every Tauri call from `settings_get_all` for no benefit.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    pub passthrough_truehd: bool,
    pub passthrough_dtshd: bool,
    pub passthrough_dtsx: bool,
    pub passthrough_atmos: bool,
    pub passthrough_ac3: bool,
    pub passthrough_dts: bool,
    pub passthrough_eac3: bool,
    pub force_hw_decode: bool,
    pub tunneling: bool,
}

// PRD §F-016 §7 Display + PRD §5 Logging together pack four independent
// boolean toggles into this view; a state-machine refactor would change
// the shape of every Tauri call from `settings_get_all` for no benefit.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayView {
    pub tile_size: String,
    pub focus_animation: bool,
    pub nsfw: bool,
    pub input_override: String,
    pub high_contrast: bool,
    /// PRD §5 Logging — when `true`, the runtime tracing filter is
    /// switched to `debug`. Default `false`.
    pub advanced_logging: bool,
}

// ---- platform-aware defaults ------------------------------------------------

/// Platform marker passed to [`load_view`] so the cache size defaults pick
/// the right per-platform value. The Tauri command surface always resolves
/// the live platform; tests inject explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    Linux,
    Android,
}

impl HostPlatform {
    /// Resolve the active platform at compile time. Anything that isn't
    /// Android compiles down to `Linux` because the Tauri host only ships
    /// to those two targets in v1 (PRD §F-018).
    pub const fn current() -> Self {
        if cfg!(target_os = "android") {
            Self::Android
        } else {
            Self::Linux
        }
    }

    pub const fn cache_default_gib(self) -> u32 {
        match self {
            Self::Linux => CACHE_DEFAULT_LINUX_GIB,
            Self::Android => CACHE_DEFAULT_ANDROID_GIB,
        }
    }

    pub const fn cache_max_gib(self) -> u32 {
        match self {
            Self::Linux => CACHE_SIZE_LINUX_MAX_GIB,
            Self::Android => CACHE_SIZE_ANDROID_MAX_GIB,
        }
    }
}

// ---- read helpers -----------------------------------------------------------

async fn read_string(db: &Db, key: &str) -> Result<Option<String>, String> {
    db.kv_get(key).await.map_err(|e| e.to_string())
}

async fn read_bool(db: &Db, key: &str, default: bool) -> Result<bool, String> {
    Ok(match read_string(db, key).await? {
        None => default,
        Some(v) => parse_bool(&v, default),
    })
}

async fn read_f64(db: &Db, key: &str, default: f64) -> Result<f64, String> {
    Ok(match read_string(db, key).await? {
        None => default,
        Some(v) => v.parse::<f64>().unwrap_or(default),
    })
}

async fn read_u32(db: &Db, key: &str, default: u32) -> Result<u32, String> {
    Ok(match read_string(db, key).await? {
        None => default,
        Some(v) => v.parse::<u32>().unwrap_or(default),
    })
}

async fn read_string_or(db: &Db, key: &str, default: &str) -> Result<String, String> {
    Ok(read_string(db, key)
        .await?
        .unwrap_or_else(|| default.to_string()))
}

async fn read_json_string_list(db: &Db, key: &str) -> Result<Vec<String>, String> {
    let Some(raw) = read_string(db, key).await? else {
        return Ok(Vec::new());
    };
    serde_json::from_str::<Vec<String>>(&raw)
        .map_err(|e| format!("{key} is not a JSON string array (got `{raw}`): {e}"))
}

/// Tolerant boolean parser — accepts the canonical `"true"` / `"false"`
/// strings we write plus the integer `"1"` / `"0"` shorthand for forward
/// compatibility with any settings written by `kv_set` directly.
fn parse_bool(s: &str, default: bool) -> bool {
    match s {
        "true" | "1" => true,
        "false" | "0" => false,
        _ => default,
    }
}

// ---- view loader ------------------------------------------------------------

/// Read the full settings tree, applying defaults for absent keys. The
/// `cache_path_default` is supplied by the caller because path resolution
/// lives in `crate::paths` and depends on the Tauri `AppHandle`.
pub async fn load_view(
    db: &Db,
    platform: HostPlatform,
    cache_path_default: &str,
) -> Result<SettingsView, String> {
    let api_keys = ApiKeysView {
        tmdb: read_string_or(db, TMDB_API_KEY, "").await?,
        trakt: read_string_or(db, TRAKT_API_KEY, "").await?,
        tvdb: read_string_or(db, TVDB_API_KEY, "").await?,
        fanart: read_string_or(db, FANART_API_KEY, "").await?,
    };
    let language = LanguageView {
        metadata_primary: read_string_or(db, META_PRIMARY_LANG_KEY, "").await?,
        metadata_fallback: read_json_string_list(db, META_FALLBACK_LANGS_KEY).await?,
        ui: read_string_or(db, UI_LANG_KEY, "").await?,
    };
    let cache = CacheView {
        path: read_string_or(db, CACHE_PATH_KEY, cache_path_default).await?,
        size_gib: read_u32(db, CACHE_SIZE_GIB_KEY, platform.cache_default_gib()).await?,
        min_gib: CACHE_SIZE_MIN_GIB,
        max_gib: platform.cache_max_gib(),
    };
    let buffer = BufferView {
        safety_margin_s: read_f64(db, BUFFER_SAFETY_MARGIN_S_KEY, SAFETY_MARGIN_S).await?,
        prebuffer_target_s: read_f64(db, BUFFER_PREBUFFER_TARGET_S_KEY, PREBUFFER_TARGET_S).await?,
        piece_high_s: read_f64(db, BUFFER_PIECE_HIGH_S_KEY, PIECE_PRIORITY_HIGH_WINDOW_S).await?,
        piece_med_s: read_f64(db, BUFFER_PIECE_MED_S_KEY, PIECE_PRIORITY_MED_WINDOW_S).await?,
        recompute_interval_s: read_f64(db, BUFFER_RECOMPUTE_INTERVAL_S_KEY, RECOMPUTE_INTERVAL_S)
            .await?,
    };
    let player = PlayerView {
        passthrough_truehd: read_bool(db, PLAYER_PASSTHROUGH_TRUEHD_KEY, true).await?,
        passthrough_dtshd: read_bool(db, PLAYER_PASSTHROUGH_DTSHD_KEY, true).await?,
        passthrough_dtsx: read_bool(db, PLAYER_PASSTHROUGH_DTSX_KEY, true).await?,
        passthrough_atmos: read_bool(db, PLAYER_PASSTHROUGH_ATMOS_KEY, true).await?,
        passthrough_ac3: read_bool(db, PLAYER_PASSTHROUGH_AC3_KEY, true).await?,
        passthrough_dts: read_bool(db, PLAYER_PASSTHROUGH_DTS_KEY, true).await?,
        passthrough_eac3: read_bool(db, PLAYER_PASSTHROUGH_EAC3_KEY, true).await?,
        force_hw_decode: read_bool(db, PLAYER_FORCE_HW_DECODE_KEY, true).await?,
        tunneling: read_bool(db, PLAYER_TUNNELING_KEY, true).await?,
    };
    let display = DisplayView {
        tile_size: read_string_or(db, DISPLAY_TILE_SIZE_KEY, "medium").await?,
        focus_animation: read_bool(db, DISPLAY_FOCUS_ANIMATION_KEY, true).await?,
        nsfw: read_bool(db, DISPLAY_NSFW_KEY, false).await?,
        input_override: read_string_or(db, DISPLAY_INPUT_OVERRIDE_KEY, "auto").await?,
        high_contrast: read_bool(db, DISPLAY_HIGH_CONTRAST_KEY, false).await?,
        advanced_logging: read_bool(db, DISPLAY_ADVANCED_LOGGING_KEY, false).await?,
    };
    Ok(SettingsView {
        api_keys,
        language,
        cache,
        buffer,
        player,
        display,
    })
}

/// Validate a setting key + value pair before persisting. Returns an
/// `Err(String)` with a human-actionable message when the value violates
/// the PRD-locked bounds (e.g. cache size out of `[1, max]` GiB, fallback
/// chain length > 3, tile size not in the allowed set). Returns Ok with
/// the normalized value (trimmed, JSON-canonicalized) on success.
pub fn validate_setting(key: &str, value: &str, platform: HostPlatform) -> Result<String, String> {
    match key {
        TMDB_API_KEY | TRAKT_API_KEY | TVDB_API_KEY | FANART_API_KEY => {
            // API keys are opaque strings; we only trim whitespace.
            Ok(value.trim().to_string())
        }
        UI_LANG_KEY => {
            let v = value.trim();
            if v.is_empty() || v == "en" || v == "fr" {
                Ok(v.to_string())
            } else {
                Err(format!("UI language `{v}` is not supported (en / fr)"))
            }
        }
        META_PRIMARY_LANG_KEY | CACHE_PATH_KEY => Ok(value.trim().to_string()),
        META_FALLBACK_LANGS_KEY => {
            // Must parse as a JSON string array; cap at META_FALLBACK_MAX.
            let parsed: Vec<String> = serde_json::from_str(value)
                .map_err(|e| format!("fallback langs must be a JSON string array: {e}"))?;
            if parsed.len() > META_FALLBACK_MAX {
                return Err(format!(
                    "fallback chain capped at {META_FALLBACK_MAX} entries (got {})",
                    parsed.len()
                ));
            }
            serde_json::to_string(&parsed).map_err(|e| e.to_string())
        }
        CACHE_SIZE_GIB_KEY => {
            let n: u32 = value
                .parse()
                .map_err(|_| format!("cache size must be an integer GiB (got `{value}`)"))?;
            let max = platform.cache_max_gib();
            if !(CACHE_SIZE_MIN_GIB..=max).contains(&n) {
                return Err(format!(
                    "cache size must be in [{CACHE_SIZE_MIN_GIB}, {max}] GiB (got {n})"
                ));
            }
            Ok(n.to_string())
        }
        BUFFER_SAFETY_MARGIN_S_KEY
        | BUFFER_PREBUFFER_TARGET_S_KEY
        | BUFFER_PIECE_HIGH_S_KEY
        | BUFFER_PIECE_MED_S_KEY
        | BUFFER_RECOMPUTE_INTERVAL_S_KEY => {
            let n: f64 = value.parse().map_err(|_| {
                format!("buffer setting `{key}` must be a number of seconds (got `{value}`)")
            })?;
            if !n.is_finite() || n <= 0.0 {
                return Err(format!(
                    "buffer setting `{key}` must be > 0 seconds (got {n})"
                ));
            }
            Ok(n.to_string())
        }
        PLAYER_PASSTHROUGH_TRUEHD_KEY
        | PLAYER_PASSTHROUGH_DTSHD_KEY
        | PLAYER_PASSTHROUGH_DTSX_KEY
        | PLAYER_PASSTHROUGH_ATMOS_KEY
        | PLAYER_PASSTHROUGH_AC3_KEY
        | PLAYER_PASSTHROUGH_DTS_KEY
        | PLAYER_PASSTHROUGH_EAC3_KEY
        | PLAYER_FORCE_HW_DECODE_KEY
        | PLAYER_TUNNELING_KEY
        | DISPLAY_FOCUS_ANIMATION_KEY
        | DISPLAY_NSFW_KEY
        | DISPLAY_HIGH_CONTRAST_KEY
        | DISPLAY_ADVANCED_LOGGING_KEY => {
            // Boolean settings — coerce to canonical string form.
            match value {
                "true" | "1" => Ok("true".to_string()),
                "false" | "0" => Ok("false".to_string()),
                _ => Err(format!("boolean setting `{key}` requires `true`/`false`")),
            }
        }
        DISPLAY_TILE_SIZE_KEY => match value {
            "small" | "medium" | "large" => Ok(value.to_string()),
            other => Err(format!(
                "tile size must be small/medium/large (got `{other}`)"
            )),
        },
        DISPLAY_INPUT_OVERRIDE_KEY => match value {
            "auto" | "touch" | "dpad" | "kbm" => Ok(value.to_string()),
            other => Err(format!(
                "input profile must be auto/touch/dpad/kbm (got `{other}`)"
            )),
        },
        _ => Err(format!("`{key}` is not a recognized settings key")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_view_returns_defaults_for_empty_db() {
        let db = Db::open_in_memory().await.unwrap();
        let view = load_view(&db, HostPlatform::Linux, "/tmp/kino")
            .await
            .unwrap();
        assert_eq!(view.api_keys.tmdb, "");
        assert_eq!(view.api_keys.trakt, "");
        assert_eq!(view.cache.path, "/tmp/kino");
        assert_eq!(view.cache.size_gib, CACHE_DEFAULT_LINUX_GIB);
        assert_eq!(view.cache.max_gib, CACHE_SIZE_LINUX_MAX_GIB);
        assert!((view.buffer.safety_margin_s - SAFETY_MARGIN_S).abs() < f64::EPSILON);
        assert!((view.buffer.prebuffer_target_s - PREBUFFER_TARGET_S).abs() < f64::EPSILON);
        assert!(view.player.passthrough_truehd);
        assert!(view.player.force_hw_decode);
        assert!(view.player.tunneling);
        assert_eq!(view.display.tile_size, "medium");
        assert!(view.display.focus_animation);
        assert!(!view.display.nsfw);
        assert_eq!(view.display.input_override, "auto");
        assert!(!view.display.high_contrast);
        assert!(!view.display.advanced_logging);
        assert!(view.language.metadata_fallback.is_empty());
    }

    #[tokio::test]
    async fn load_view_reads_persisted_advanced_logging() {
        let db = Db::open_in_memory().await.unwrap();
        db.kv_set(DISPLAY_ADVANCED_LOGGING_KEY, "true")
            .await
            .unwrap();
        let view = load_view(&db, HostPlatform::Linux, "/tmp/kino")
            .await
            .unwrap();
        assert!(view.display.advanced_logging);
    }

    #[test]
    fn validate_setting_normalizes_advanced_logging() {
        assert_eq!(
            validate_setting(DISPLAY_ADVANCED_LOGGING_KEY, "1", HostPlatform::Linux).unwrap(),
            "true"
        );
        assert_eq!(
            validate_setting(DISPLAY_ADVANCED_LOGGING_KEY, "false", HostPlatform::Linux).unwrap(),
            "false"
        );
        assert!(
            validate_setting(DISPLAY_ADVANCED_LOGGING_KEY, "yep", HostPlatform::Linux).is_err()
        );
    }

    #[tokio::test]
    async fn load_view_picks_android_defaults() {
        let db = Db::open_in_memory().await.unwrap();
        let view = load_view(&db, HostPlatform::Android, "/data/kino")
            .await
            .unwrap();
        assert_eq!(view.cache.size_gib, CACHE_DEFAULT_ANDROID_GIB);
        assert_eq!(view.cache.max_gib, CACHE_SIZE_ANDROID_MAX_GIB);
    }

    #[tokio::test]
    async fn load_view_reads_persisted_values() {
        let db = Db::open_in_memory().await.unwrap();
        db.kv_set(TMDB_API_KEY, "k1").await.unwrap();
        db.kv_set(META_PRIMARY_LANG_KEY, "fr").await.unwrap();
        db.kv_set(META_FALLBACK_LANGS_KEY, "[\"en\",\"de\"]")
            .await
            .unwrap();
        db.kv_set(CACHE_SIZE_GIB_KEY, "12").await.unwrap();
        db.kv_set(DISPLAY_TILE_SIZE_KEY, "large").await.unwrap();
        db.kv_set(DISPLAY_INPUT_OVERRIDE_KEY, "dpad").await.unwrap();
        db.kv_set(PLAYER_FORCE_HW_DECODE_KEY, "false")
            .await
            .unwrap();
        db.kv_set(BUFFER_SAFETY_MARGIN_S_KEY, "45.5").await.unwrap();
        let view = load_view(&db, HostPlatform::Linux, "/tmp/kino")
            .await
            .unwrap();
        assert_eq!(view.api_keys.tmdb, "k1");
        assert_eq!(view.language.metadata_primary, "fr");
        assert_eq!(view.language.metadata_fallback, vec!["en", "de"]);
        assert_eq!(view.cache.size_gib, 12);
        assert_eq!(view.display.tile_size, "large");
        assert_eq!(view.display.input_override, "dpad");
        assert!(!view.player.force_hw_decode);
        assert!((view.buffer.safety_margin_s - 45.5).abs() < f64::EPSILON);
    }

    #[test]
    fn validate_setting_normalizes_booleans() {
        assert_eq!(
            validate_setting(PLAYER_FORCE_HW_DECODE_KEY, "1", HostPlatform::Linux).unwrap(),
            "true"
        );
        assert_eq!(
            validate_setting(DISPLAY_NSFW_KEY, "false", HostPlatform::Linux).unwrap(),
            "false"
        );
        assert!(validate_setting(DISPLAY_NSFW_KEY, "yes", HostPlatform::Linux).is_err());
    }

    #[test]
    fn validate_setting_enforces_tile_size_set() {
        assert!(validate_setting(DISPLAY_TILE_SIZE_KEY, "huge", HostPlatform::Linux).is_err());
        assert!(validate_setting(DISPLAY_TILE_SIZE_KEY, "large", HostPlatform::Linux).is_ok());
    }

    #[test]
    fn validate_setting_enforces_input_override_set() {
        assert!(
            validate_setting(DISPLAY_INPUT_OVERRIDE_KEY, "trackball", HostPlatform::Linux).is_err()
        );
        assert_eq!(
            validate_setting(DISPLAY_INPUT_OVERRIDE_KEY, "kbm", HostPlatform::Linux).unwrap(),
            "kbm"
        );
    }

    #[test]
    fn validate_setting_enforces_cache_size_bounds() {
        assert!(validate_setting(CACHE_SIZE_GIB_KEY, "0", HostPlatform::Linux).is_err());
        assert!(validate_setting(CACHE_SIZE_GIB_KEY, "100", HostPlatform::Linux).is_ok());
        assert!(validate_setting(CACHE_SIZE_GIB_KEY, "101", HostPlatform::Linux).is_err());
        // Android cap differs.
        assert!(validate_setting(CACHE_SIZE_GIB_KEY, "51", HostPlatform::Android).is_err());
        assert!(validate_setting(CACHE_SIZE_GIB_KEY, "50", HostPlatform::Android).is_ok());
    }

    #[test]
    fn validate_setting_enforces_fallback_chain_cap() {
        assert!(validate_setting(
            META_FALLBACK_LANGS_KEY,
            "[\"en\",\"fr\",\"es\"]",
            HostPlatform::Linux
        )
        .is_ok());
        assert!(validate_setting(
            META_FALLBACK_LANGS_KEY,
            "[\"en\",\"fr\",\"es\",\"de\"]",
            HostPlatform::Linux
        )
        .is_err());
        assert!(
            validate_setting(META_FALLBACK_LANGS_KEY, "not-json", HostPlatform::Linux).is_err()
        );
    }

    #[test]
    fn validate_setting_rejects_ui_lang_outside_supported() {
        assert!(validate_setting(UI_LANG_KEY, "en", HostPlatform::Linux).is_ok());
        assert!(validate_setting(UI_LANG_KEY, "fr", HostPlatform::Linux).is_ok());
        assert!(validate_setting(UI_LANG_KEY, "", HostPlatform::Linux).is_ok());
        assert!(validate_setting(UI_LANG_KEY, "ja", HostPlatform::Linux).is_err());
    }

    #[test]
    fn validate_setting_rejects_unknown_key() {
        assert!(validate_setting("nonsense.flag", "true", HostPlatform::Linux).is_err());
    }

    #[test]
    fn known_settings_keys_are_unique() {
        let mut sorted: Vec<&&str> = KNOWN_SETTINGS_KEYS.iter().collect();
        sorted.sort();
        let mut dedup = sorted.clone();
        dedup.dedup();
        assert_eq!(
            sorted.len(),
            dedup.len(),
            "duplicate key in KNOWN_SETTINGS_KEYS"
        );
    }

    #[test]
    fn known_settings_keys_do_not_collide_with_system_keys() {
        for key in KNOWN_SETTINGS_KEYS {
            assert_ne!(*key, kino_core::db::INSTALL_ID_KEY);
            assert_ne!(*key, "addons.bootstrap_done");
        }
    }
}
