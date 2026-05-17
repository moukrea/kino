//! kino Tauri 2 host library.
//!
//! On desktop targets this crate is consumed via the thin `kino-app` binary
//! in `src/main.rs`. On Android Tauri builds a `cdylib` from this same
//! library and drives it via the JNI shim that the Tauri Android template
//! generates under `src-tauri/gen/android/`.
//!
//! As of Session 003 the host opens the `SQLite` database (PRD §F-002) at
//! setup time, stores the [`Db`] handle in app state, and registers the
//! KV / Continue Watching / addons commands. Future sessions extend the
//! `invoke_handler` registry as their features land.

mod commands;
mod paths;

use kino_core::Db;
use tauri::Manager;

/// Entry point shared by the desktop binary and the Android `cdylib`.
///
/// # Panics
///
/// Panics if the Tauri runtime fails to start (e.g. the webview cannot be
/// initialized on the host OS). This is the standard Tauri host pattern; a
/// failure here means the user is on an unsupported platform and there is
/// no recovery short of crashing with a clear backtrace.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let db_path = paths::db_path(&handle).map_err(|e| {
                tracing::error!(error = %e, "failed to resolve DB path");
                e
            })?;
            tracing::info!(path = %db_path.display(), "opening kino database");
            let db = tauri::async_runtime::block_on(Db::open(&db_path)).map_err(|e| {
                tracing::error!(error = %e, "failed to open kino database");
                e.to_string()
            })?;
            // PRD §F-007: install Cinemeta as a non-removable default
            // addon on first launch. Runs once (gated by a settings
            // marker); failure here doesn't block startup — the user can
            // retry from Settings → Addons.
            tauri::async_runtime::block_on(commands::bootstrap_default_addons(&db));
            app.manage(db);
            tracing::info!("kino host started (PRD §F-002 persistence ready)");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::kv_get,
            commands::kv_set,
            commands::install_id,
            commands::cw_list,
            commands::cw_upsert,
            commands::cw_delete,
            commands::addons_list,
            commands::addons_insert,
            commands::addons_delete,
            commands::addons_set_enabled,
            commands::addons_reorder,
            commands::test_tmdb,
            commands::test_trakt,
            commands::test_tvdb,
            commands::test_fanart,
            commands::get_trending,
            commands::resolve_artwork,
            commands::check_availability,
            commands::get_recommended_addons,
            commands::install_addon,
            commands::uninstall_addon,
            commands::set_addon_order,
        ])
        .run(tauri::generate_context!())
        .expect("kino: error while running tauri application");
}
