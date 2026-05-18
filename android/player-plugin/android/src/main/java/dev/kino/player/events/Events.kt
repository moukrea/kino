package dev.kino.player.events

import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject

/**
 * Event wire DTOs matching the Rust [`PlayerEvent`] tagged enum (see
 * `crates/kino-player/src/event.rs`).
 *
 * The Rust side decodes via `serde(tag = "kind")`, so every JSObject
 * we emit MUST carry a `kind` field plus the camelCase fields for the
 * specific variant.
 *
 * This file is intentionally a small set of factory helpers — using
 * `JSObject` directly keeps the bridge zero-allocation per event and
 * matches the surface the Tauri runtime expects.
 */
object PlayerEventFactory {

    /** Build a `position` event from the ExoPlayer playback clock. */
    fun position(positionS: Double, durationS: Double, paused: Boolean): JSObject =
        JSObject().apply {
            put("kind", "position")
            put("positionS", positionS)
            put("durationS", durationS)
            put("paused", paused)
        }

    /** Build a `state` event. `state` is one of the PlayerState camelCase strings. */
    fun state(state: String): JSObject =
        JSObject().apply {
            put("kind", "state")
            put("state", state)
        }

    /** Build a `tracks` event from a pre-built TrackList JSObject. */
    fun tracks(tracks: JSObject): JSObject =
        JSObject().apply {
            put("kind", "tracks")
            put("tracks", tracks)
        }

    /** Build the terminal `exit` event with the final position. */
    fun exit(positionS: Double, durationS: Double, reachedEof: Boolean): JSObject =
        JSObject().apply {
            put("kind", "exit")
            put("positionS", positionS)
            put("durationS", durationS)
            put("reachedEof", reachedEof)
        }

    /** Build the terminal `error` event with a message. */
    fun error(message: String): JSObject =
        JSObject().apply {
            put("kind", "error")
            put("message", message)
        }
}

/** Build the inner `TrackList` shape used by [PlayerEventFactory.tracks]. */
object TrackListBuilder {

    fun build(audio: List<JSObject>, subtitles: List<JSObject>): JSObject =
        JSObject().apply {
            put("audio", JSArray(audio))
            put("subtitles", JSArray(subtitles))
        }

    fun audioTrack(
        id: Long,
        title: String?,
        language: String?,
        codec: String?,
        channels: Int?,
        isDefault: Boolean,
        isSelected: Boolean
    ): JSObject = JSObject().apply {
        put("id", id)
        title?.let { put("title", it) } ?: put("title", JSObject.NULL)
        language?.let { put("language", it) } ?: put("language", JSObject.NULL)
        codec?.let { put("codec", it) } ?: put("codec", JSObject.NULL)
        channels?.let { put("channels", it) } ?: put("channels", JSObject.NULL)
        put("isDefault", isDefault)
        put("isSelected", isSelected)
    }

    fun subtitleTrack(
        id: Long,
        title: String?,
        language: String?,
        codec: String?,
        isDefault: Boolean,
        isForced: Boolean,
        isSelected: Boolean
    ): JSObject = JSObject().apply {
        put("id", id)
        title?.let { put("title", it) } ?: put("title", JSObject.NULL)
        language?.let { put("language", it) } ?: put("language", JSObject.NULL)
        codec?.let { put("codec", it) } ?: put("codec", JSObject.NULL)
        put("isDefault", isDefault)
        put("isForced", isForced)
        put("isSelected", isSelected)
    }
}
