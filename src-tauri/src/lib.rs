//! kino Tauri 2 host library.
//!
//! On desktop targets this crate is consumed via the thin `kino-app` binary
//! in `src/main.rs`. On Android Tauri builds a `cdylib` from this same
//! library and drives it via the JNI shim that the Tauri Android template
//! generates under `src-tauri/gen/android/`.
//!
//! Session 002 (PRD F-001) wires only the placeholder host. Future sessions
//! mount Tauri commands and the axum HTTP server (F-013) here.

/// Entry point shared by the desktop binary and the Android `cdylib`.
///
/// All real backend wiring (Tauri commands, axum, librqbit, sqlx) lands in
/// later sessions. For F-001 we install the `tracing` subscriber and run
/// the default Tauri builder so the placeholder home renders.
///
/// # Panics
///
/// Panics if the Tauri runtime fails to start (e.g. the webview cannot be
/// initialized on the host OS). This is the standard Tauri host pattern; a
/// failure here means the user is on an unsupported platform and there is
/// no recovery short of crashing with a clear backtrace.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // INFO-level logging by default; promoted to DEBUG via the "advanced
    // logging" settings toggle landing in F-016. Logs to stderr today;
    // rolling file output (PRD §5 Logging) lands with the persistence
    // feature.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tauri::Builder::default()
        .setup(|_app| {
            tracing::info!("kino host started (PRD §F-001 placeholder)");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("kino: error while running tauri application");
}
