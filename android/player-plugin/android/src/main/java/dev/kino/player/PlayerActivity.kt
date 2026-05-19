package dev.kino.player

import android.content.pm.ActivityInfo
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.ImageButton
import android.widget.PopupMenu
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.ui.PlayerView
import app.tauri.plugin.JSObject
import dev.kino.player.events.PlayerEventFactory
import dev.kino.player.events.TrackListBuilder

/**
 * PRD §F-015 Android playback activity (ADR-010).
 *
 * Owns the [ExoPlayer] instance, the [PlayerView] surface, and the
 * controls overlay (audio / subtitle / info / back buttons in
 * `kino_player_controls.xml`; the play/pause + seek bar are reused
 * from Media3's controller). Pulls open-args from
 * [PlayerSession] on `onCreate`, forwards every meaningful
 * [Player.Listener] callback into [PlayerSession.enqueue] for the
 * Rust driver to drain.
 *
 * ## Lifecycle (PRD §F-015):
 *
 *  - **Launch**: PlayerPlugin starts the activity via Intent; the
 *    activity reads [PlayerSession.takeOpenArgs] in `onCreate`.
 *  - **Back press**: pause the player, emit a terminal `exit` event,
 *    then `finish()`. The plugin's `close()` invoke also triggers the
 *    same path via [requestExit].
 *  - **onPause**: pause the player, save the current position (the
 *    position is captured by the next position-tick emission so the
 *    Rust side persists it via F-012's CW writer).
 *  - **onResume**: stay paused; the user explicitly presses play to
 *    resume. PRD §F-015 explicitly locks this — re-entering the app
 *    shouldn't auto-resume.
 *  - **Player error**: emit `player:error`, render the error overlay,
 *    wait for back press.
 *
 * ## ExoPlayer config (PRD §F-015):
 *
 *  - `DefaultRenderersFactory`:
 *      - `EXTENSION_RENDERER_MODE_OFF` (hardware preferred via
 *        [DvAwareCodecSelector], which wraps `MediaCodecSelector.DEFAULT`
 *        for non-DV mimetypes and, for `video/dolby-vision`, filters
 *        the candidate list to decoders whose
 *        `CodecCapabilities.profileLevels` declare a DV profile entry
 *        — implements the PRD §F-015 "force DV-capable decoder" rule).
 *      - DV passthrough enabled (relies on hardware decoder picking
 *        up `dvhe.05` / `dvhe.08` profiles per [Capabilities]).
 *      - `setEnableAudioTrackPlaybackParams(true)` + audio attributes
 *        flagged as movie content so the system's audio session
 *        treats us as a media playback foreground client.
 *  - `DefaultTrackSelector`:
 *      - Default audio language = the PRD §F-016 primary preference
 *        (passed via OpenArgs.fileName for now; full audio-lang
 *        preference plumbing lands in a follow-up).
 *      - Subtitle parsers: tier-1 + tier-2 via [SubtitleSupport].
 *  - `setAudioAttributes(..., handleAudioFocus = true)`: standard
 *    media playback focus handling.
 *  - Tunneling enabled on Android TV when [Capabilities.tunneling]
 *    returns a supported MIME.
 */
@UnstableApi
class PlayerActivity : AppCompatActivity() {

    companion object {
        private const val TAG = "kino.PlayerActivity"

        /** Position-tick cadence (PRD §8 `PLAYER_POSITION_INTERVAL_S = 5s`). */
        private const val POSITION_TICK_MS: Long = 5_000

        /** Faster cadence used during seeks / state transitions. */
        private const val POSITION_TICK_FAST_MS: Long = 250
    }

    private lateinit var playerView: PlayerView
    private lateinit var loadingSpinner: ProgressBar
    private lateinit var errorText: TextView
    private lateinit var infoPanel: ScrollView
    private lateinit var infoText: TextView
    private lateinit var titleText: TextView
    private lateinit var backBtn: ImageButton
    private lateinit var audioTrackBtn: ImageButton
    private lateinit var subtitleTrackBtn: ImageButton
    private lateinit var infoBtn: ImageButton

    private var player: ExoPlayer? = null
    private var trackSelector: DefaultTrackSelector? = null
    private var capabilities: Capabilities.Snapshot? = null

    private val handler = Handler(Looper.getMainLooper())
    private val positionTickRunnable = object : Runnable {
        override fun run() {
            emitPositionTick()
            handler.postDelayed(this, POSITION_TICK_MS)
        }
    }

    private var lastEmittedState: Int = -1
    private var sessionToken: String = ""
    private var fileName: String? = null
    private var durationHintS: Double? = null
    private var hasFinishedNormally: Boolean = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        installImmersiveDecor()
        setContentView(R.layout.activity_player)

        playerView = findViewById(R.id.kino_player_view)
        loadingSpinner = findViewById(R.id.kino_player_loading)
        errorText = findViewById(R.id.kino_player_error_text)
        infoPanel = findViewById(R.id.kino_info_panel)
        infoText = findViewById(R.id.kino_info_text)
        titleText = findViewById(R.id.kino_title_text)
        backBtn = findViewById(R.id.kino_back_btn)
        audioTrackBtn = findViewById(R.id.kino_audio_track_btn)
        subtitleTrackBtn = findViewById(R.id.kino_subtitle_track_btn)
        infoBtn = findViewById(R.id.kino_info_btn)

        backBtn.setOnClickListener { requestExit(reachedEof = false) }
        infoBtn.setOnClickListener { toggleInfoPanel() }
        audioTrackBtn.setOnClickListener { showAudioTrackPicker(it) }
        subtitleTrackBtn.setOnClickListener { showSubtitleTrackPicker(it) }

        val args = PlayerSession.takeOpenArgs()
        if (args == null) {
            Log.e(TAG, "no open args; finishing")
            requestExit(reachedEof = false)
            return
        }
        sessionToken = args.token
        fileName = args.fileName
        durationHintS = args.durationHintS
        titleText.text = args.fileName ?: getString(R.string.kino_player_activity_label)

        PlayerSession.setActivity(this)
        capabilities = Capabilities.probe(this)
        infoText.text = describeSession(args)

        initPlayer(args)
    }

    private fun installImmersiveDecor() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            window.setDecorFitsSystemWindows(false)
            window.insetsController?.apply {
                hide(WindowInsets.Type.systemBars())
                systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            }
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = (
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                    or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                    or View.SYSTEM_UI_FLAG_FULLSCREEN
                    or View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                )
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    private fun initPlayer(args: PlayerSession.OpenSessionArgs) {
        val renderersFactory = DefaultRenderersFactory(this)
            .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_OFF)
            .setEnableDecoderFallback(true)
            .setMediaCodecSelector(DvAwareCodecSelector)
            .setEnableAudioTrackPlaybackParams(true)

        val selector = DefaultTrackSelector(this).apply {
            // Default to surfacing all tracks; the user picks via the
            // overlay menus.
            val params = buildUponParameters()
                .setForceLowestBitrate(false)
                .setTunnelingEnabled(capabilities?.tunnelingMimeType != null)
                .build()
            parameters = params
        }
        trackSelector = selector

        val player = ExoPlayer.Builder(this, renderersFactory)
            .setTrackSelector(selector)
            .setSeekBackIncrementMs(10_000)
            .setSeekForwardIncrementMs(10_000)
            .build()
        player.setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(C.USAGE_MEDIA)
                .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                .build(),
            /* handleAudioFocus = */ true,
        )
        player.addListener(playerListener)

        val item = MediaItem.Builder()
            .setUri(args.url)
            .setMimeType(MimeTypes.APPLICATION_MP4) // probe; ExoPlayer will reprobe
            .build()
        player.setMediaItem(item, /* startPositionMs = */ (args.resumePositionS * 1000).toLong())
        player.prepare()
        player.playWhenReady = true

        playerView.player = player
        this.player = player
        loadingSpinner.visibility = View.VISIBLE

        // Emit the initial Loading state so the host's overlay shows
        // the spinner before the first state callback fires.
        emitState(Player.STATE_BUFFERING, isPlaying = false)
        handler.postDelayed(positionTickRunnable, POSITION_TICK_FAST_MS)
    }

    /**
     * ExoPlayer event listener — translates Media3 callbacks into
     * PRD-locked [PlayerEvent]s queued for the Rust driver.
     */
    private val playerListener = object : Player.Listener {
        override fun onPlaybackStateChanged(playbackState: Int) {
            lastEmittedState = playbackState
            when (playbackState) {
                Player.STATE_READY -> {
                    loadingSpinner.visibility = View.GONE
                    errorText.visibility = View.GONE
                }
                Player.STATE_BUFFERING -> {
                    loadingSpinner.visibility = View.VISIBLE
                }
                Player.STATE_ENDED -> {
                    requestExit(reachedEof = true)
                }
                Player.STATE_IDLE -> { /* no-op */ }
            }
            emitState(playbackState, isPlaying = player?.isPlaying ?: false)
            emitPositionTick()
        }

        override fun onIsPlayingChanged(isPlaying: Boolean) {
            emitState(lastEmittedState, isPlaying)
            emitPositionTick()
        }

        override fun onTracksChanged(tracks: Tracks) {
            emitTracks(tracks)
        }

        override fun onPlayerError(error: PlaybackException) {
            Log.e(TAG, "ExoPlayer error", error)
            errorText.text = error.localizedMessage ?: getString(R.string.kino_player_error_generic)
            errorText.visibility = View.VISIBLE
            loadingSpinner.visibility = View.GONE
            PlayerSession.enqueue(PlayerEventFactory.error(error.localizedMessage ?: error.javaClass.simpleName))
        }

        override fun onPositionDiscontinuity(
            oldPosition: Player.PositionInfo,
            newPosition: Player.PositionInfo,
            reason: Int,
        ) {
            emitPositionTick()
        }
    }

    private fun emitPositionTick() {
        val p = player ?: return
        val durationMs = p.duration.coerceAtLeast(0)
        val positionMs = p.currentPosition.coerceAtLeast(0)
        val paused = !p.isPlaying && p.playbackState != Player.STATE_BUFFERING
        PlayerSession.enqueue(
            PlayerEventFactory.position(
                positionS = positionMs / 1000.0,
                durationS = durationMs / 1000.0,
                paused = paused,
            )
        )
    }

    private fun emitState(playbackState: Int, isPlaying: Boolean) {
        val name = when (playbackState) {
            Player.STATE_IDLE -> "idle"
            Player.STATE_BUFFERING -> if (player?.duration ?: 0 > 0) "buffering" else "loading"
            Player.STATE_READY -> if (isPlaying) "playing" else "paused"
            Player.STATE_ENDED -> "ended"
            else -> "idle"
        }
        PlayerSession.enqueue(PlayerEventFactory.state(name))
    }

    private fun emitTracks(tracks: Tracks) {
        val audio = mutableListOf<JSObject>()
        val subs = mutableListOf<JSObject>()
        for (group in tracks.groups) {
            for (i in 0 until group.length) {
                if (!group.isTrackSupported(i)) continue
                val format = group.getTrackFormat(i)
                val codec = format.codecs ?: format.sampleMimeType
                val isSelected = group.isTrackSelected(i)
                val isDefault = (format.selectionFlags and C.SELECTION_FLAG_DEFAULT) != 0
                val isForced = (format.selectionFlags and C.SELECTION_FLAG_FORCED) != 0
                val id = ((group.type.toLong() and 0xFF) shl 32) or (i.toLong() and 0xFFFFFFFFL)
                when (group.type) {
                    C.TRACK_TYPE_AUDIO -> {
                        audio.add(TrackListBuilder.audioTrack(
                            id = id,
                            title = format.label,
                            language = format.language,
                            codec = codec,
                            channels = if (format.channelCount > 0) format.channelCount else null,
                            isDefault = isDefault,
                            isSelected = isSelected,
                        ))
                    }
                    C.TRACK_TYPE_TEXT -> {
                        subs.add(TrackListBuilder.subtitleTrack(
                            id = id,
                            title = format.label,
                            language = format.language,
                            codec = codec,
                            isDefault = isDefault,
                            isForced = isForced,
                            isSelected = isSelected,
                        ))
                    }
                    else -> {}
                }
            }
        }
        PlayerSession.enqueue(PlayerEventFactory.tracks(TrackListBuilder.build(audio, subs)))
    }

    /** Called from PlayerPlugin via main-thread dispatch. */
    fun setPaused(paused: Boolean) {
        runOnUiThread {
            player?.playWhenReady = !paused
        }
    }

    /** Called from PlayerPlugin via main-thread dispatch. */
    fun seekTo(positionS: Double) {
        runOnUiThread {
            player?.seekTo((positionS * 1000).toLong())
            emitPositionTick()
        }
    }

    /**
     * Called from PlayerPlugin via main-thread dispatch. `null`
     * disables audio entirely.
     */
    fun selectAudio(trackId: Long?) {
        runOnUiThread {
            applyTrackOverride(C.TRACK_TYPE_AUDIO, trackId)
        }
    }

    /**
     * Called from PlayerPlugin via main-thread dispatch. `null`
     * disables subtitles entirely.
     */
    fun selectSubtitle(trackId: Long?) {
        runOnUiThread {
            applyTrackOverride(C.TRACK_TYPE_TEXT, trackId)
        }
    }

    /**
     * Called from PlayerPlugin via main-thread dispatch. Triggers the
     * exit path with reachedEof=false.
     */
    fun requestClose() {
        runOnUiThread { requestExit(reachedEof = false) }
    }

    private fun applyTrackOverride(trackType: Int, trackId: Long?) {
        val selector = trackSelector ?: return
        val player = player ?: return
        val params = selector.buildUponParameters()
        params.clearOverridesOfType(trackType)
        if (trackId == null) {
            params.setTrackTypeDisabled(trackType, true)
        } else {
            params.setTrackTypeDisabled(trackType, false)
            val groups = player.currentTracks.groups
            val matched = groups.firstOrNull { g ->
                g.type == trackType && (0 until g.length).any { idx ->
                    val id = ((g.type.toLong() and 0xFF) shl 32) or (idx.toLong() and 0xFFFFFFFFL)
                    id == trackId
                }
            }
            if (matched != null) {
                val idx = (0 until matched.length).firstOrNull { i ->
                    val id = ((matched.type.toLong() and 0xFF) shl 32) or (i.toLong() and 0xFFFFFFFFL)
                    id == trackId
                } ?: 0
                params.addOverride(TrackSelectionOverride(matched.mediaTrackGroup, listOf(idx)))
            }
        }
        selector.setParameters(params.build())
    }

    private fun showAudioTrackPicker(anchor: View) {
        val groups = player?.currentTracks?.groups ?: return
        val menu = PopupMenu(this, anchor)
        var idx = 1
        menu.menu.add(0, 0, 0, getString(R.string.kino_player_subtitles_off))
        val mapping = mutableMapOf<Int, Long>()
        for (group in groups) {
            if (group.type != C.TRACK_TYPE_AUDIO) continue
            for (i in 0 until group.length) {
                if (!group.isTrackSupported(i)) continue
                val format = group.getTrackFormat(i)
                val label = listOfNotNull(format.label, format.language, format.codecs).joinToString(" / ").ifEmpty { "Audio $i" }
                val id = ((group.type.toLong() and 0xFF) shl 32) or (i.toLong() and 0xFFFFFFFFL)
                mapping[idx] = id
                menu.menu.add(0, idx, idx, label)
                idx++
            }
        }
        menu.setOnMenuItemClickListener { item ->
            if (item.itemId == 0) {
                selectAudio(null)
            } else {
                selectAudio(mapping[item.itemId])
            }
            true
        }
        menu.show()
    }

    private fun showSubtitleTrackPicker(anchor: View) {
        val groups = player?.currentTracks?.groups ?: return
        val menu = PopupMenu(this, anchor)
        var idx = 1
        menu.menu.add(0, 0, 0, getString(R.string.kino_player_subtitles_off))
        val mapping = mutableMapOf<Int, Long>()
        for (group in groups) {
            if (group.type != C.TRACK_TYPE_TEXT) continue
            for (i in 0 until group.length) {
                if (!group.isTrackSupported(i)) continue
                val format = group.getTrackFormat(i)
                val tier = SubtitleSupport.label(format.sampleMimeType)
                val label = listOfNotNull(format.label, format.language, tier).joinToString(" / ").ifEmpty { "Subtitle $i" }
                val id = ((group.type.toLong() and 0xFF) shl 32) or (i.toLong() and 0xFFFFFFFFL)
                mapping[idx] = id
                menu.menu.add(0, idx, idx, label)
                idx++
            }
        }
        menu.setOnMenuItemClickListener { item ->
            if (item.itemId == 0) {
                selectSubtitle(null)
            } else {
                selectSubtitle(mapping[item.itemId])
            }
            true
        }
        menu.show()
    }

    private fun toggleInfoPanel() {
        if (infoPanel.visibility == View.VISIBLE) {
            infoPanel.visibility = View.GONE
        } else {
            infoText.text = describeSession(currentArgs())
            infoPanel.visibility = View.VISIBLE
        }
    }

    private fun currentArgs(): PlayerSession.OpenSessionArgs {
        return PlayerSession.OpenSessionArgs(
            token = sessionToken,
            url = player?.currentMediaItem?.localConfiguration?.uri?.toString() ?: "",
            resumePositionS = (player?.currentPosition ?: 0) / 1000.0,
            fileName = fileName,
            durationHintS = durationHintS,
        )
    }

    private fun describeSession(args: PlayerSession.OpenSessionArgs): String {
        val sb = StringBuilder()
        sb.appendLine("Token: ${args.token}")
        sb.appendLine("URL: ${args.url}")
        sb.appendLine("File: ${args.fileName ?: "(unknown)"}")
        sb.appendLine("Resume: ${"%.2f".format(args.resumePositionS)}s")
        sb.appendLine()
        capabilities?.let { sb.appendLine(Capabilities.describe(it)) }
        return sb.toString()
    }

    private fun requestExit(reachedEof: Boolean) {
        if (hasFinishedNormally) return
        hasFinishedNormally = true
        handler.removeCallbacks(positionTickRunnable)
        val p = player
        val positionS = (p?.currentPosition ?: 0) / 1000.0
        val durationS = (p?.duration?.coerceAtLeast(0) ?: 0) / 1000.0
        PlayerSession.enqueue(
            PlayerEventFactory.exit(
                positionS = positionS,
                durationS = durationS,
                reachedEof = reachedEof,
            )
        )
        try {
            p?.release()
        } catch (t: Throwable) {
            Log.w(TAG, "ExoPlayer release failed", t)
        }
        player = null
        PlayerSession.setActivity(null)
        finish()
    }

    override fun onPause() {
        super.onPause()
        // PRD §F-015: pause-on-recent-apps; the position-tick will save
        // the current position via the Rust bridge.
        player?.let {
            if (it.playWhenReady) {
                it.playWhenReady = false
                emitPositionTick()
            }
        }
    }

    override fun onResume() {
        super.onResume()
        installImmersiveDecor()
    }

    override fun onDestroy() {
        handler.removeCallbacksAndMessages(null)
        if (!hasFinishedNormally) {
            // The OS killed us before requestExit ran (low-memory,
            // user swiped from recents, etc.). Emit a final exit
            // event with the current position so the Rust bridge can
            // still persist CW.
            val p = player
            val positionS = (p?.currentPosition ?: 0) / 1000.0
            val durationS = (p?.duration?.coerceAtLeast(0) ?: 0) / 1000.0
            PlayerSession.enqueue(
                PlayerEventFactory.exit(
                    positionS = positionS,
                    durationS = durationS,
                    reachedEof = false,
                )
            )
            try {
                p?.release()
            } catch (t: Throwable) {
                Log.w(TAG, "ExoPlayer release failed in onDestroy", t)
            }
            player = null
            PlayerSession.setActivity(null)
        }
        super.onDestroy()
    }

    override fun onBackPressed() {
        requestExit(reachedEof = false)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent?): Boolean {
        // D-pad center / Enter — toggle play/pause. Media3's
        // controller catches some of these natively but the activity
        // root sees them first when controls are hidden.
        return when (keyCode) {
            KeyEvent.KEYCODE_DPAD_CENTER,
            KeyEvent.KEYCODE_ENTER,
            KeyEvent.KEYCODE_SPACE,
            KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE -> {
                player?.let { it.playWhenReady = !it.playWhenReady }
                true
            }
            KeyEvent.KEYCODE_MEDIA_PLAY -> {
                player?.playWhenReady = true
                true
            }
            KeyEvent.KEYCODE_MEDIA_PAUSE -> {
                player?.playWhenReady = false
                true
            }
            KeyEvent.KEYCODE_MEDIA_REWIND,
            KeyEvent.KEYCODE_DPAD_LEFT -> {
                player?.let { it.seekTo((it.currentPosition - 10_000).coerceAtLeast(0)) }
                true
            }
            KeyEvent.KEYCODE_MEDIA_FAST_FORWARD,
            KeyEvent.KEYCODE_DPAD_RIGHT -> {
                player?.let { it.seekTo((it.currentPosition + 10_000).coerceAtMost(it.duration.coerceAtLeast(0))) }
                true
            }
            else -> super.onKeyDown(keyCode, event)
        }
    }
}
