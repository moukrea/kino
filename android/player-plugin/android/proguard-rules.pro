# PRD §F-015 Android plugin proguard rules.
#
# Tauri's mobile plugin runtime invokes `@Command`-annotated methods
# via reflection; their signatures must survive R8 / proguard. The
# `app.tauri:tauri-android` module's consumer-rules already cover the
# Tauri base classes; this file pins the kino-player-specific surface.

# Keep the plugin class + every @Command method.
-keep class dev.kino.player.PlayerPlugin { *; }
# Keep PlayerActivity (referenced via class literal in PlayerPlugin).
-keep class dev.kino.player.PlayerActivity { *; }
# Keep the @InvokeArg data classes — Tauri uses reflection to populate
# them from the incoming JSObject.
-keep class dev.kino.player.args.** { *; }
# Keep PlayerEvent wire types — they're serialised back to Rust through
# the Tauri JSObject layer.
-keep class dev.kino.player.events.** { *; }
