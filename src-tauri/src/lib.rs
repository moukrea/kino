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

mod cache_fs;
mod commands;
mod logs;
mod paths;
mod settings;

use kino_core::Db;
use tauri::Manager;
use tracing_appender::non_blocking::WorkerGuard;

/// Holder for the rolling-file-appender worker guard. The guard must outlive
/// the process so the appender thread flushes buffered log lines on exit;
/// stashing it in Tauri's managed state is the idiomatic pattern.
#[allow(dead_code)] // held purely for its Drop side effect
struct LogGuard(WorkerGuard);

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
    // Subscriber init lives in setup() rather than at function entry so the
    // PRD §F-016 §8 file appender can be wired in the SAME init call as the
    // stderr layer (a second `try_init` after the first wins NOTHING — the
    // global default subscriber is set-once). Until setup runs, the
    // tracing-log shim drops events; this is acceptable because we haven't
    // resolved the config dir yet and the AppHandle is the canonical source.
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let db_path = paths::db_path(&handle).map_err(|e| {
                eprintln!("kino: failed to resolve DB path: {e}");
                e
            })?;
            // PRD §F-016 §8 About → "Export logs button". Mount a daily
            // rotating file appender alongside the DB so the user has
            // something to share with support. Failure to install the file
            // appender (e.g. config dir not writable) falls back to
            // stderr-only logging so the app still boots.
            install_subscriber(app, &handle);
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
            commands::get_trending_pools,
            commands::get_weekly_trending,
            commands::list_home_catalogs,
            commands::get_title_detail,
            commands::get_streams,
            commands::resolve_artwork,
            commands::check_availability,
            commands::get_recommended_addons,
            commands::install_addon,
            commands::uninstall_addon,
            commands::set_addon_order,
            commands::search,
            commands::recent_searches_list,
            commands::recent_searches_upsert,
            commands::recent_searches_clear,
            commands::settings_get_all,
            commands::settings_set,
            commands::settings_reset_defaults,
            commands::cache_usage_bytes,
            commands::cache_clear,
            commands::export_logs,
            commands::get_app_info,
        ])
        .run(tauri::generate_context!())
        .expect("kino: error while running tauri application");
}

/// Install the global `tracing` subscriber. When the per-platform config
/// directory is writable, a daily-rotating file appender is layered next to
/// stderr so the F-016 §8 Export-Logs button has artifacts to ship. When the
/// directory isn't available (rare — read-only home dir, sandbox), stderr
/// alone still keeps the app debuggable.
fn install_subscriber<R: tauri::Runtime>(app: &tauri::App<R>, handle: &tauri::AppHandle<R>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    match paths::app_config_dir(handle)
        .and_then(|root| logs::install_file_appender(&root).map_err(|e| e.to_string()))
    {
        Ok((non_blocking, guard)) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .try_init();
            app.manage(LogGuard(guard));
        }
        Err(_) => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .try_init();
        }
    }
}
