package dev.kino.player

import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi

/**
 * PRD §F-015 subtitle parser tiers.
 *
 *  - **Tier 1 (required)**: SRT, WebVTT, SSA/ASS basic dialogue lines
 *    + positioning. Media3 ships parsers for all three out of the
 *    box (`SubripParser`, `WebvttParser`, `SsaParser`).
 *
 *  - **Tier 2 (best-effort, non-blocking)**: PGS image subtitles
 *    (`PgsParser`) and ASS with advanced effects (karaoke, complex
 *    animations).  Media3's SSA parser handles basic dialogue +
 *    positioning but explicitly does not implement the full
 *    SubStation Alpha override-tag surface; the PRD locks this as
 *    out-of-scope for v1.
 *
 * The set of MIME types we report as "supported" to the platform's
 * MediaItem builder controls which sidecar subtitle files ExoPlayer
 * is willing to ingest. Embedded subtitle tracks are picked up
 * automatically by the extractor regardless of this set.
 */
@UnstableApi
object SubtitleSupport {

    /** PRD §F-015 tier-1 subtitle MIME types (required). */
    val TIER1_MIMES: List<String> = listOf(
        MimeTypes.APPLICATION_SUBRIP,
        MimeTypes.TEXT_VTT,
        MimeTypes.TEXT_SSA,
    )

    /** PRD §F-015 tier-2 subtitle MIME types (best-effort). */
    val TIER2_MIMES: List<String> = listOf(
        MimeTypes.APPLICATION_PGS,
    )

    /** Every subtitle MIME ExoPlayer will accept from a sidecar file. */
    val ACCEPTED_SIDECAR_MIMES: List<String> = TIER1_MIMES + TIER2_MIMES

    /** True when the MIME is a PRD tier-1 parser. */
    fun isTier1(mime: String?): Boolean = mime != null && TIER1_MIMES.any { it.equals(mime, ignoreCase = true) }

    /** True when the MIME is a PRD tier-2 parser. */
    fun isTier2(mime: String?): Boolean = mime != null && TIER2_MIMES.any { it.equals(mime, ignoreCase = true) }

    /**
     * Friendly label for the info panel. Returns `"srt"` / `"webvtt"`
     * / `"ssa-ass"` / `"pgs"` / `"unknown"`.
     */
    fun label(mime: String?): String = when {
        mime == null -> "unknown"
        mime.equals(MimeTypes.APPLICATION_SUBRIP, ignoreCase = true) -> "srt"
        mime.equals(MimeTypes.TEXT_VTT, ignoreCase = true) -> "webvtt"
        mime.equals(MimeTypes.TEXT_SSA, ignoreCase = true) -> "ssa-ass"
        mime.equals(MimeTypes.APPLICATION_PGS, ignoreCase = true) -> "pgs"
        else -> mime
    }
}
