package dev.kino.player

import android.util.Log
import app.tauri.plugin.JSObject
import java.util.ArrayDeque

/**
 * Inter-component event queue + active session reference (PRD §F-015,
 * Android side).
 *
 * Three pieces of state live here as a singleton:
 *
 *  1. The active [PlayerActivity] reference (or `null` between
 *     sessions). The Tauri plugin uses it to dispatch
 *     control commands (`setPaused`, `seek`, `selectAudioTrack`, ...)
 *     onto the activity's main thread.
 *
 *  2. The active session's open arguments (URL, resume position, file
 *     name). The activity reads them in `onCreate` rather than the
 *     plugin packing them into Intent extras — keeps the activity
 *     resilient to recreate-from-stack flows (which would otherwise
 *     replay the original URL after a config change).
 *
 *  3. The event queue. PlayerActivity calls [enqueue] each time
 *     ExoPlayer emits a position / state / tracks update; the Tauri
 *     plugin's `drain_events` command pops the queue and returns it
 *     to the Rust driver, which then rebroadcasts.
 *
 * The queue is bounded to 256 entries; on overflow the OLDEST entry is
 * dropped and the [overflowed] flag is set. The Rust driver logs a
 * warning on the next drain. This is acceptable because the steady-
 * state event rate is one position tick per 5s (PRD §8); only a
 * pathologically stalled Rust poller could fill the queue.
 *
 * Thread safety: every method synchronises on `this` so the plugin
 * (called on the Tauri command thread) and the activity (called on
 * the main thread) can talk safely.
 */
object PlayerSession {

    private const val TAG = "PlayerSession"
    private const val QUEUE_CAPACITY = 256

    @Volatile
    private var activity: PlayerActivity? = null

    @Volatile
    private var openArgs: OpenSessionArgs? = null

    private val queue: ArrayDeque<JSObject> = ArrayDeque()
    private var overflowed: Boolean = false

    /** Immutable snapshot of the args used to start a session. */
    data class OpenSessionArgs(
        val token: String,
        val url: String,
        val resumePositionS: Double,
        val fileName: String?,
        val durationHintS: Double?
    )

    @Synchronized
    fun setOpenArgs(args: OpenSessionArgs) {
        openArgs = args
    }

    @Synchronized
    fun takeOpenArgs(): OpenSessionArgs? = openArgs

    @Synchronized
    fun setActivity(a: PlayerActivity?) {
        activity = a
    }

    @Synchronized
    fun getActivity(): PlayerActivity? = activity

    /**
     * Enqueue an event for the next [drain] call. Called from
     * PlayerActivity on the main thread.
     */
    @Synchronized
    fun enqueue(event: JSObject) {
        if (queue.size >= QUEUE_CAPACITY) {
            queue.pollFirst()
            overflowed = true
            Log.w(TAG, "event queue overflowed; dropping oldest entry")
        }
        queue.addLast(event)
    }

    /**
     * Drain every queued event into a list. Resets the overflow flag.
     * Called from the plugin on the Tauri command thread.
     */
    @Synchronized
    fun drain(): DrainResult {
        val events = ArrayList<JSObject>(queue.size)
        while (queue.isNotEmpty()) {
            events.add(queue.pollFirst())
        }
        val o = overflowed
        overflowed = false
        return DrainResult(events, o)
    }

    /**
     * Clear all session state. Called when the session ends so a
     * subsequent `open()` starts clean.
     */
    @Synchronized
    fun reset() {
        activity = null
        openArgs = null
        queue.clear()
        overflowed = false
    }

    data class DrainResult(val events: List<JSObject>, val overflowed: Boolean)
}
