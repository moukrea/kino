package dev.kino.player

import android.content.Context
import android.media.AudioFormat
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.view.Display
import androidx.media3.common.C
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.common.util.Util
import androidx.media3.exoplayer.audio.AudioCapabilities

/**
 * Capability probes used to configure ExoPlayer per the locked PRD
 * §F-015 hardware-decoder / DV / HDR / audio-passthrough rules.
 *
 * Each probe is a static `Capabilities` method returning a `Snapshot`
 * the PlayerActivity attaches to the info panel and uses to drive
 * `DefaultRenderersFactory` configuration:
 *
 *  - [dolbyVisionProfiles] enumerates which DV profiles the device has
 *    a hardware decoder for. PRD locks profile 5 + 8.1 as required;
 *    profile 7 is best-effort (ADR-022). The activity uses this to
 *    pick a DV-capable `MediaCodecSelector` override when the stream
 *    carries a `dvhe.05` / `dvhe.08` codec descriptor.
 *
 *  - [hdrCapabilities] reads `Display.HdrCapabilities`. ExoPlayer's
 *    `DefaultRenderersFactory` honors the result automatically — this
 *    probe is informational, surfaced in the info panel.
 *
 *  - [audioPassthrough] reads `AudioCapabilities.getCapabilities` to
 *    figure out which of TrueHD / DTS-HD MA / DTS-X / E-AC3 JOC / AC3
 *    / EAC3 / DTS the sink supports. The activity flips the
 *    `setAudioAttributes(passthrough=true)` flag and falls back to
 *    decode-and-mix for unsupported codecs (per-codec toggles live in
 *    PRD §F-016 audio settings).
 *
 *  - [tunneling] returns true when `Util.getTunnelingV21SupportedMimeType`
 *    succeeds for HEVC / H.264 / VP9 / AV1. Activity sets
 *    `setTunnelingEnabled(true)` on Android TV when this is true.
 */
@UnstableApi
object Capabilities {

    /** PRD §F-015: locked DV profile codec descriptors. */
    private val DV_PROFILE_5_LEVELS = listOf(
        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheStn,
    )
    private val DV_PROFILE_81_LEVELS = listOf(
        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheSt,
    )

    /** PRD §F-015 audio-codec set (locked). */
    enum class Codec(val mime: String) {
        TRUEHD(MimeTypes.AUDIO_TRUEHD),
        DTS_HD_MA(MimeTypes.AUDIO_DTS_HD),
        DTS_X(MimeTypes.AUDIO_DTS_X),
        EAC3_JOC(MimeTypes.AUDIO_E_AC3_JOC),
        EAC3(MimeTypes.AUDIO_E_AC3),
        AC3(MimeTypes.AUDIO_AC3),
        DTS(MimeTypes.AUDIO_DTS),
    }

    data class Snapshot(
        val dolbyVision: Set<String>,
        val hdr: Set<String>,
        val audio: Set<Codec>,
        val tunnelingMimeType: String?,
        val decoderInfo: Map<String, String>,
    )

    fun probe(context: Context): Snapshot {
        val dolbyVision = dolbyVisionProfiles()
        val hdr = hdrCapabilities(context)
        val audio = audioPassthrough(context)
        val tunneling = tunneling()
        val decoders = decoderRoster()
        return Snapshot(dolbyVision, hdr, audio, tunneling, decoders)
    }

    fun dolbyVisionProfiles(): Set<String> {
        val result = mutableSetOf<String>()
        val list = MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos
        for (info in list) {
            if (info.isEncoder) continue
            for (type in info.supportedTypes) {
                if (!type.equals(MimeTypes.VIDEO_DOLBY_VISION, ignoreCase = true)) continue
                val caps = info.getCapabilitiesForType(type) ?: continue
                for (pl in caps.profileLevels) {
                    when (pl.profile) {
                        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheStn -> {
                            result.add("profile5")
                        }
                        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheSt -> {
                            result.add("profile8.1")
                        }
                        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheDtb -> {
                            result.add("profile7")
                        }
                        else -> {}
                    }
                }
            }
        }
        return result
    }

    fun hdrCapabilities(context: Context): Set<String> {
        val display: Display = context.getSystemService(android.hardware.display.DisplayManager::class.java)
            ?.displays
            ?.firstOrNull()
            ?: return emptySet()
        val result = mutableSetOf<String>()
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.N) return result
        val caps = display.hdrCapabilities ?: return result
        for (type in caps.supportedHdrTypes) {
            when (type) {
                Display.HdrCapabilities.HDR_TYPE_HDR10 -> result.add("hdr10")
                Display.HdrCapabilities.HDR_TYPE_HDR10_PLUS -> result.add("hdr10+")
                Display.HdrCapabilities.HDR_TYPE_DOLBY_VISION -> result.add("dolbyVision")
                Display.HdrCapabilities.HDR_TYPE_HLG -> result.add("hlg")
            }
        }
        return result
    }

    /**
     * Read [`AudioCapabilities`] and return the set of PRD §F-015
     * audio codecs the connected sink supports for passthrough.
     */
    fun audioPassthrough(context: Context): Set<Codec> {
        val caps = AudioCapabilities.getCapabilities(context)
        val result = mutableSetOf<Codec>()
        for (codec in Codec.values()) {
            val encoding = mimeToEncoding(codec.mime)
            if (encoding == AudioFormat.ENCODING_INVALID) continue
            if (caps.supportsEncoding(encoding)) result.add(codec)
        }
        return result
    }

    private fun mimeToEncoding(mime: String): Int = when (mime) {
        MimeTypes.AUDIO_AC3 -> AudioFormat.ENCODING_AC3
        MimeTypes.AUDIO_E_AC3 -> AudioFormat.ENCODING_E_AC3
        MimeTypes.AUDIO_E_AC3_JOC -> AudioFormat.ENCODING_E_AC3_JOC
        MimeTypes.AUDIO_DTS -> AudioFormat.ENCODING_DTS
        MimeTypes.AUDIO_DTS_HD -> AudioFormat.ENCODING_DTS_HD
        MimeTypes.AUDIO_DTS_X -> if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            AudioFormat.ENCODING_DTS_UHD_P2
        } else AudioFormat.ENCODING_INVALID
        MimeTypes.AUDIO_TRUEHD -> AudioFormat.ENCODING_DOLBY_TRUEHD
        else -> AudioFormat.ENCODING_INVALID
    }

    /**
     * PRD §F-015: tunneling on Android TV when the platform reports a
     * supported MIME via [`Util.getTunnelingV21SupportedMimeType`]. We
     * probe each of the main video codecs in PRD §8's quality table.
     */
    fun tunneling(): String? {
        if (Build.VERSION.SDK_INT < 21) return null
        for (mime in listOf(
            MimeTypes.VIDEO_DOLBY_VISION,
            MimeTypes.VIDEO_H265,
            MimeTypes.VIDEO_H264,
            MimeTypes.VIDEO_VP9,
            MimeTypes.VIDEO_AV1,
        )) {
            val format = MediaFormat()
            format.setString(MediaFormat.KEY_MIME, mime)
            format.setFeatureEnabled(MediaCodecInfo.CodecCapabilities.FEATURE_TunneledPlayback, true)
            val name = MediaCodecList(MediaCodecList.REGULAR_CODECS).findDecoderForFormat(format)
            if (!name.isNullOrEmpty()) return mime
        }
        return null
    }

    /** Map of MIME → first hardware-preferred decoder name. */
    fun decoderRoster(): Map<String, String> {
        val out = mutableMapOf<String, String>()
        val mimes = listOf(
            MimeTypes.VIDEO_DOLBY_VISION,
            MimeTypes.VIDEO_H265,
            MimeTypes.VIDEO_H264,
            MimeTypes.VIDEO_VP9,
            MimeTypes.VIDEO_AV1,
        )
        for (mime in mimes) {
            val format = MediaFormat()
            format.setString(MediaFormat.KEY_MIME, mime)
            val name = MediaCodecList(MediaCodecList.REGULAR_CODECS).findDecoderForFormat(format)
            if (!name.isNullOrEmpty()) out[mime] = name
        }
        return out
    }

    /** True if the current sink supports any of the PRD-locked audio codecs. */
    fun hasAnyPassthrough(context: Context): Boolean = audioPassthrough(context).isNotEmpty()

    /** True when running on Android TV (the system-feature flag). */
    fun isAndroidTv(context: Context): Boolean {
        return context.packageManager.hasSystemFeature("android.software.leanback")
    }

    /** Build a human-readable info-panel summary from [Snapshot] + a media format. */
    fun describe(snapshot: Snapshot): String {
        val sb = StringBuilder()
        sb.appendLine("Dolby Vision profiles: ${snapshot.dolbyVision.sorted().ifEmpty { listOf("none") }}")
        sb.appendLine("HDR: ${snapshot.hdr.sorted().ifEmpty { listOf("sdr") }}")
        sb.appendLine("Audio passthrough: ${snapshot.audio.map { it.name.lowercase() }.sorted().ifEmpty { listOf("pcm only") }}")
        sb.appendLine("Tunneling MIME: ${snapshot.tunnelingMimeType ?: "(unsupported)"}")
        sb.appendLine("Decoders:")
        for ((mime, name) in snapshot.decoderInfo) {
            sb.appendLine("  - $mime → $name")
        }
        return sb.toString().trim()
    }

    /** Translate an ExoPlayer track-renderer C.TYPE_* into a PRD-shape `kind`. */
    fun trackKind(type: Int): String = when (type) {
        C.TRACK_TYPE_AUDIO -> "audio"
        C.TRACK_TYPE_TEXT -> "subtitle"
        C.TRACK_TYPE_VIDEO -> "video"
        else -> "other"
    }
}
