package dev.kino.player

import android.media.MediaCodecInfo as AndroidMediaCodecInfo
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.mediacodec.MediaCodecInfo
import androidx.media3.exoplayer.mediacodec.MediaCodecSelector
import androidx.media3.exoplayer.mediacodec.MediaCodecUtil

/**
 * PRD §F-015 (Android): on Dolby Vision content
 * (`MimeTypes.VIDEO_DOLBY_VISION`, which Media3 emits when the track
 * carries a `dvhe.05` / `dvhe.08` / `dvh1.05` / `dvh1.08` codec
 * descriptor), filter [MediaCodecSelector.DEFAULT]'s decoder list
 * down to codecs whose
 * [AndroidMediaCodecInfo.CodecCapabilities.profileLevels] declare a
 * `DolbyVisionProfileDvhe*` entry. Non-DV mime types delegate to
 * `DEFAULT` unchanged so HEVC / H.264 / VP9 / AV1 / audio /
 * subtitle decoder selection keeps the stock Media3 behavior the
 * info-panel snapshot already documents.
 *
 * Backs PRD §F-015 line: "For DV content (profile 5/8.1 detected in
 * stream metadata), force selection of a DV-capable decoder."
 * Companion to [Capabilities.dolbyVisionProfiles] which enumerates
 * the device's static DV-decoder roster for the info panel; this
 * selector is the runtime enforcement that ExoPlayer's
 * `MediaCodecVideoRenderer` consults when choosing a decoder for a
 * DV track. Profile-5-vs-profile-8.1 disambiguation is left to
 * ExoPlayer's standard `Format.codecs` → `CodecProfileLevel` matching
 * inside the codec; this selector's job is to prune non-DV-capable
 * decoders from the candidate set so the matching stage cannot fall
 * back to one.
 *
 * If filtering would empty the list (no DV-capable decoder reported
 * by `MediaCodecList` at all — e.g. an emulator image without HW
 * codecs) the unfiltered list is returned so playback still attempts;
 * surfacing the failure to the user via `onPlayerError` is more useful
 * than a silent "no renderer" stall.
 */
@UnstableApi
object DvAwareCodecSelector : MediaCodecSelector {

    /**
     * DV profile constants the PRD §F-015 lock requires:
     *   - `DolbyVisionProfileDvheStn` = profile 5 (PRD-required)
     *   - `DolbyVisionProfileDvheSt`  = profile 8.1 (PRD-required)
     *   - `DolbyVisionProfileDvheDtb` = profile 7 (best-effort, ADR-022)
     * Mirrors the set [Capabilities.dolbyVisionProfiles] already probes
     * for the info-panel snapshot; the AVC-based DvavSe / Per / Pen
     * profiles are out of PRD scope and intentionally excluded here.
     */
    private val DV_PROFILE_CONSTANTS = setOf(
        AndroidMediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheStn,
        AndroidMediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheSt,
        AndroidMediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheDtb,
    )

    @Throws(MediaCodecUtil.DecoderQueryException::class)
    override fun getDecoderInfos(
        mimeType: String,
        requiresSecureDecoder: Boolean,
        requiresTunnelingDecoder: Boolean,
    ): List<MediaCodecInfo> {
        val base = MediaCodecSelector.DEFAULT.getDecoderInfos(
            mimeType,
            requiresSecureDecoder,
            requiresTunnelingDecoder,
        )
        if (!mimeType.equals(MimeTypes.VIDEO_DOLBY_VISION, ignoreCase = true)) {
            return base
        }
        val dvOnly = base.filter { info -> isDvCapable(info) }
        return if (dvOnly.isEmpty()) base else dvOnly
    }

    private fun isDvCapable(info: MediaCodecInfo): Boolean {
        val caps = info.capabilities ?: return false
        val profileLevels = caps.profileLevels ?: return false
        for (pl in profileLevels) {
            if (pl.profile in DV_PROFILE_CONSTANTS) return true
        }
        return false
    }
}
