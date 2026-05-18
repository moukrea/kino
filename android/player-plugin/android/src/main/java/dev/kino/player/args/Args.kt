package dev.kino.player.args

import app.tauri.annotation.InvokeArg

/**
 * Argument DTOs matching the Rust-side [`models`](../../../../src/models.rs)
 * shapes. Each `@InvokeArg` class is populated by Tauri's plugin
 * runtime from the `JSObject` payload the Rust driver passes to
 * `run_mobile_plugin(name, payload)`.
 *
 * Field names MUST match the camelCase keys the Rust side serialises.
 */

/** `open(...)` — start a new playback session. */
@InvokeArg
class OpenArgs {
    /** Stable playback token (host's `start_playback` UUID). */
    lateinit var token: String

    /** Stream URL (typically `http://127.0.0.1:PORT/stream/<token>`). */
    lateinit var url: String

    /** Resume offset in seconds. `0.0` for fresh playback. */
    var resumePositionS: Double = 0.0

    /** Optional file name shown in the info panel + window title. */
    var fileName: String? = null

    /** Optional duration hint in seconds (UI sizing only). */
    var durationHintS: Double? = null
}

/** `set_paused(paused)` — toggle pause state. */
@InvokeArg
class SetPausedArgs {
    var paused: Boolean = false
}

/** `seek(positionS)` — jump to an absolute offset. */
@InvokeArg
class SeekArgs {
    var positionS: Double = 0.0
}

/**
 * `select_audio_track(trackId)` / `select_subtitle_track(trackId)` —
 * choose a track by backend id. `null` disables the track.
 */
@InvokeArg
class SelectTrackArgs {
    var trackId: Long? = null
}
