//! Per-platform filesystem layout for kino's app data (PRD §3 Storage layout).
//!
//! | Target  | DB / config root                           |
//! |---------|--------------------------------------------|
//! | Linux   | `$XDG_CONFIG_HOME/kino/` (≈ `~/.config/kino`) |
//! | Android | Tauri's `app_config_dir()` → `Context.filesDir` |
//!
//! The PRD pins the Linux dir to `kino/` rather than the bundle identifier
//! `dev.kino.app/` that Tauri's default `app_config_dir()` would yield, so
//! Linux resolves explicitly via `dirs::config_dir()`. Android delegates to
//! the Tauri path resolver because the OS doesn't expose XDG-style env vars.

use std::path::PathBuf;

#[cfg(not(target_os = "linux"))]
use tauri::Manager;
use tauri::{AppHandle, Runtime};

/// Application config root for the current target.
#[cfg(target_os = "linux")]
pub fn app_config_dir<R: Runtime>(_app: &AppHandle<R>) -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "could not resolve XDG_CONFIG_HOME / ~/.config".to_string())?;
    Ok(base.join("kino"))
}

#[cfg(not(target_os = "linux"))]
pub fn app_config_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("could not resolve app config dir: {e}"))
}

/// Full path to the `SQLite` database file.
pub fn db_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app_config_dir(app)?.join(kino_core::Db::db_filename()))
}
