# Consumer proguard rules — applied to apps depending on this library.
# PRD §F-015 Android: the host app's R8 / proguard pass must keep the
# plugin entry points so the Tauri runtime can resolve them by name.

-keep class dev.kino.player.PlayerPlugin { *; }
-keep class dev.kino.player.PlayerActivity { *; }
-keep class dev.kino.player.args.** { *; }
-keep class dev.kino.player.events.** { *; }
