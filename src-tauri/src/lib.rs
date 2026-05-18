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

use std::sync::Arc;

use kino_core::Db;
use tauri::Manager;
use tracing_appender::non_blocking::WorkerGuard;

use commands::{LogFilterHandle, PlayerRuntime, TorrentRuntime};

/// Holder for the rolling-file-appender worker guard. The guard must outlive
/// the process so the appender thread flushes buffered log lines on exit;
/// stashing it in Tauri's managed state is the idiomatic pattern.
#[allow(dead_code)] // held purely for its Drop side effect
struct LogGuard(WorkerGuard);

/// PRD §5 Reliability: install a global panic hook that records the panic
/// message and a captured backtrace via `tracing::error!` before chaining
/// to the default hook (so the process still exits with the standard panic
/// signature and unhandled-panic exit code).
///
/// Installed at the very top of [`run`] — before the Tauri builder and
/// before [`install_subscriber`] runs — so that bootstrap panics still
/// chain through the default hook (printing to stderr with a backtrace).
/// Once the subscriber is live the same hook ALSO writes to the rolling
/// log file, giving §6B field-test crashes a backtrace artifact.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let location = info.location().map_or_else(
            || "<unknown>".to_string(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let payload = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".to_string()
        };
        tracing::error!(
            location = %location,
            payload = %payload,
            backtrace = %backtrace,
            "kino panic"
        );
        default_hook(info);
    }));
}

/// Entry point shared by the desktop binary and the Android `cdylib`.
///
/// # Panics
///
/// Panics if the Tauri runtime fails to start (e.g. the webview cannot be
/// initialized on the host OS). This is the standard Tauri host pattern; a
/// failure here means the user is on an unsupported platform and there is
/// no recovery short of crashing with a clear backtrace.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::too_many_lines)] // setup() block lists every Tauri command + manages every state.
pub fn run() {
    // PRD §5 Reliability: install the panic hook BEFORE the Tauri builder
    // so a panic during bootstrap is captured even though no tracing
    // subscriber is live yet (the chained default hook still prints to
    // stderr with a backtrace).
    install_panic_hook();
    // Subscriber init lives in setup() rather than at function entry so the
    // PRD §F-016 §8 file appender can be wired in the SAME init call as the
    // stderr layer (a second `try_init` after the first wins NOTHING — the
    // global default subscriber is set-once). Until setup runs, the
    // tracing-log shim drops events; this is acceptable because we haven't
    // resolved the config dir yet and the AppHandle is the canonical source.
    tauri::Builder::default()
        // PRD §F-015 Android: register the `kino-player` Tauri 2 mobile
        // plugin so `commands::spawn_platform_player` can resolve the
        // Android `PlayerHandle` impl out of app state. On Linux the
        // plugin installs a `StubPlayer` that errors on every call — the
        // mpv subprocess driver wired below is what actually drives
        // Linux playback. The stub keeps the registration path uniform
        // so a future libmpv-in-process driver can land behind the same
        // plugin-as-feature-flag interface.
        .plugin(tauri_plugin_kino_player::init())
        // PRD §F-016 §4: native directory picker for the Cache → Path
        // setting. The dialog plugin's `open({ directory: true })` call
        // is invoked from the frontend "Browse…" button next to the
        // path TextField; the returned path flows back through the
        // standard `settingsSet` channel. ADR-095's text-only fallback
        // is preserved (the user can still type/paste a path); the
        // picker is a layered convenience.
        .plugin(tauri_plugin_dialog::init())
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
            // PRD §5 Logging: honour the persisted `display.advanced_logging`
            // toggle at boot — if it's on, flip the reload-aware filter from
            // the default `info` to `debug`. Failure here is non-fatal; the
            // subscriber keeps the boot-time level.
            if let Some(filter_handle) = app.try_state::<LogFilterHandle>() {
                let advanced = tauri::async_runtime::block_on(
                    db.kv_get(settings::DISPLAY_ADVANCED_LOGGING_KEY),
                )
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
                if advanced {
                    if let Err(e) = filter_handle.apply("debug") {
                        tracing::warn!(error = %e, "could not apply advanced logging on boot");
                    }
                }
            }
            // PRD §F-007: install Cinemeta as a non-removable default
            // addon on first launch. Runs once (gated by a settings
            // marker); failure here doesn't block startup — the user can
            // retry from Settings → Addons.
            tauri::async_runtime::block_on(commands::bootstrap_default_addons(&db));

            // PRD §F-013: bring up the librqbit-backed torrent engine and
            // local HTTP server. Cache root honors the user's
            // `cache.path` setting (F-016 §4) with a fall-back to
            // `cache_dir_default`. Engine init failure does NOT block
            // startup — the UI still loads, but `start_playback` will
            // return an error until the user fixes the cache dir.
            let cache_root = tauri::async_runtime::block_on(async {
                match commands::resolve_cache_path(&handle, &db).await {
                    Ok(p) => std::path::PathBuf::from(p),
                    Err(e) => {
                        tracing::warn!(error = %e, "cache path unresolved; using default");
                        paths::cache_dir_default(&handle).unwrap_or_else(|_| {
                            std::env::temp_dir().join("kino-cache")
                        })
                    }
                }
            });
            match tauri::async_runtime::block_on(TorrentRuntime::new(cache_root)) {
                Ok(runtime) => {
                    tracing::info!(
                        addr = %runtime.server.addr(),
                        "kino torrent engine + HTTP server ready"
                    );
                    app.manage(runtime);
                }
                Err(e) => {
                    tracing::error!(error = %e, "torrent runtime failed to start; playback unavailable");
                }
            }

            app.manage(db);
            // PRD §F-015: an empty PlayerRuntime is registered so the
            // first `player_open` call lazily boots the platform driver
            // (mpv on Linux; the Android Tauri plugin elsewhere). The
            // runtime is `Arc`d so command handlers and the bridge task
            // can hold cheap clones without contesting the Tauri state
            // lock.
            app.manage(Arc::new(PlayerRuntime::default()));
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
            commands::cw_record_position,
            commands::cw_remove_title,
            commands::cw_sweep,
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
            commands::start_playback,
            commands::stop_playback,
            commands::playback_status,
            commands::buffer_start_monitor,
            commands::buffer_stop_monitor,
            commands::buffer_report_position,
            commands::buffer_status,
            commands::player_open,
            commands::player_close,
            commands::player_pause,
            commands::player_seek,
            commands::player_set_audio_track,
            commands::player_set_subtitle_track,
            commands::player_status,
        ])
        .run(tauri::generate_context!())
        .expect("kino: error while running tauri application");
}

/// Install the global `tracing` subscriber. When the per-platform config
/// directory is writable, a daily-rotating file appender is layered next to
/// stderr so the F-016 §8 Export-Logs button has artifacts to ship. When the
/// directory isn't available (rare — read-only home dir, sandbox), stderr
/// alone still keeps the app debuggable.
///
/// The `EnvFilter` is wrapped in a `reload::Layer` so PRD §5 Logging's
/// "DEBUG when 'advanced logging' toggle is on in settings" can switch
/// the filter at runtime without re-initialising the subscriber. The
/// type-erased applier closure is stored as managed state under
/// [`LogFilterHandle`] so [`commands::settings_set`] and the boot-time
/// path can both flip the level.
fn install_subscriber<R: tauri::Runtime>(app: &tauri::App<R>, handle: &tauri::AppHandle<R>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::reload;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let initial = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, reload_handle) = reload::Layer::new(initial);
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    match paths::app_config_dir(handle)
        .and_then(|root| logs::install_file_appender(&root).map_err(|e| e.to_string()))
    {
        Ok((non_blocking, guard)) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false);
            let _ = tracing_subscriber::registry()
                .with(filter_layer)
                .with(stderr_layer)
                .with(file_layer)
                .try_init();
            app.manage(LogGuard(guard));
        }
        Err(_) => {
            let _ = tracing_subscriber::registry()
                .with(filter_layer)
                .with(stderr_layer)
                .try_init();
        }
    }

    // Erase the reload-handle's subscriber-stack type behind a closure so
    // app state can hold a plain `Send + Sync` value regardless of which
    // match branch above ran.
    let applier: commands::LogFilterApplier = Box::new(move |level: &str| {
        let next = EnvFilter::try_new(level).map_err(|e| format!("invalid log filter: {e}"))?;
        reload_handle
            .modify(|f| *f = next)
            .map_err(|e| format!("reload handle closed: {e}"))
    });
    app.manage(LogFilterHandle::new(applier));
}
