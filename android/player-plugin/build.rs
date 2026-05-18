//! Tauri 2 mobile-plugin build script (PRD §F-015 Android wiring).
//!
//! Declares the JS-facing command surface that bridges through the
//! Kotlin-side `PlayerPlugin`, and tells the Tauri CLI where the
//! companion Android library lives (`./android/` relative to this
//! manifest). The CLI walks the host app's dep tree, sees the `links =
//! "tauri-plugin-kino-player"` directive in our `Cargo.toml`, and emits
//! the `include(":tauri-plugin-kino-player")` plus
//! `implementation(project(":tauri-plugin-kino-player"))` lines into
//! `src-tauri/gen/android/tauri.{settings,build}.gradle.kts` at every
//! `cargo tauri android build` invocation.
//!
//! The command list MUST match the `@Command` methods declared on the
//! Kotlin-side `PlayerPlugin` class; the Tauri Kotlin runtime dispatches
//! by name.

const COMMANDS: &[&str] = &[
    "open",
    "close",
    "set_paused",
    "seek",
    "select_audio_track",
    "select_subtitle_track",
    "snapshot",
    "tracks",
    "drain_events",
    "ping",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
