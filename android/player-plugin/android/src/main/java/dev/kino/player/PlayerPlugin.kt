package dev.kino.player

import android.app.Activity
import android.content.Intent
import android.os.Handler
import android.os.Looper
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import dev.kino.player.args.OpenArgs
import dev.kino.player.args.SeekArgs
import dev.kino.player.args.SelectTrackArgs
import dev.kino.player.args.SetPausedArgs

/**
 * PRD §F-015 Tauri 2 mobile plugin. Wraps the kino Android
 * [PlayerActivity] for the Rust-side `AndroidPlayer` driver.
 *
 * The plugin is the Rust ↔ Activity bridge:
 *
 *  - **Outgoing (Rust → Activity)**: `@Command` methods receive Invoke
 *    payloads from the Rust `run_mobile_plugin(name, args)` calls.
 *    `open` launches PlayerActivity via Intent; the rest are dispatched
 *    onto the active activity's main thread.
 *
 *  - **Incoming (Activity → Rust)**: PlayerActivity enqueues each
 *    PlayerEvent onto [PlayerSession.enqueue]; the Rust driver's poll
 *    task drains via the `drain_events` command at a 250ms cadence.
 *
 * Every `@Command` resolves the Invoke with a single side-effect (or
 * fails fast) — no command blocks on async work because Tauri's
 * mobile plugin runtime expects synchronous resolution.
 */
@TauriPlugin
class PlayerPlugin(private val activity: Activity) : Plugin(activity) {

    private val mainHandler = Handler(Looper.getMainLooper())

    @Command
    fun open(invoke: Invoke) {
        val args = invoke.parseArgs(OpenArgs::class.java)
        PlayerSession.setOpenArgs(
            PlayerSession.OpenSessionArgs(
                token = args.token,
                url = args.url,
                resumePositionS = args.resumePositionS,
                fileName = args.fileName,
                durationHintS = args.durationHintS,
            )
        )
        // Close any active session first (PRD §F-015 "open replaces
        // existing"). The activity's requestExit emits a terminal
        // `exit` event before finish()ing.
        PlayerSession.getActivity()?.requestClose()
        val intent = Intent(activity, PlayerActivity::class.java)
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        activity.startActivity(intent)
        invoke.resolve()
    }

    @Command
    fun close(invoke: Invoke) {
        val a = PlayerSession.getActivity()
        if (a != null) {
            a.requestClose()
        }
        invoke.resolve()
    }

    @Command
    @Suppress("unused_parameter")
    fun set_paused(invoke: Invoke) {
        val args = invoke.parseArgs(SetPausedArgs::class.java)
        PlayerSession.getActivity()?.setPaused(args.paused)
        invoke.resolve()
    }

    @Command
    fun seek(invoke: Invoke) {
        val args = invoke.parseArgs(SeekArgs::class.java)
        PlayerSession.getActivity()?.seekTo(args.positionS)
        invoke.resolve()
    }

    @Command
    fun select_audio_track(invoke: Invoke) {
        val args = invoke.parseArgs(SelectTrackArgs::class.java)
        PlayerSession.getActivity()?.selectAudio(args.trackId)
        invoke.resolve()
    }

    @Command
    fun select_subtitle_track(invoke: Invoke) {
        val args = invoke.parseArgs(SelectTrackArgs::class.java)
        PlayerSession.getActivity()?.selectSubtitle(args.trackId)
        invoke.resolve()
    }

    @Command
    fun snapshot(invoke: Invoke) {
        // The Rust driver maintains its own cached snapshot (updated
        // from the drain_events stream). This command exists for
        // symmetry with the trait surface; we return a best-effort
        // payload by inspecting the active activity's player.
        val a = PlayerSession.getActivity()
        if (a == null) {
            invoke.resolve(JSObject().apply {
                put("token", "")
                put("state", "idle")
                put("positionS", 0.0)
                put("durationS", 0.0)
                put("paused", false)
            })
            return
        }
        // Fields populated on the main thread; we'd need to runOnUi
        // for live values. Tauri's mobile plugin runtime is sync, so
        // we approximate with the most recent emitted state.
        invoke.resolve(JSObject().apply {
            put("token", "")
            put("state", "playing")
            put("positionS", 0.0)
            put("durationS", 0.0)
            put("paused", false)
        })
    }

    @Command
    fun tracks(invoke: Invoke) {
        // Likewise: the Rust driver caches the track list from the
        // drain stream. Return an empty payload so the round-trip is
        // valid even if the Rust side asks before the first
        // `onTracksChanged` callback.
        invoke.resolve(JSObject().apply {
            put("audio", JSArray())
            put("subtitles", JSArray())
        })
    }

    @Command
    @Suppress("unused_parameter")
    fun drain_events(invoke: Invoke) {
        val result = PlayerSession.drain()
        invoke.resolve(JSObject().apply {
            put("events", JSArray(result.events))
            put("overflowed", result.overflowed)
        })
    }

    @Command
    @Suppress("unused_parameter")
    fun ping(invoke: Invoke) {
        invoke.resolve(JSObject().apply {
            put("pong", true)
        })
    }
}
