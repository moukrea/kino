//! Stream-filename parsing. Regex set and detection order are locked by PRD §8.
//!
//! ADR-024 commits us to a regex-set implementation here. Each tag class
//! (quality / HDR / codec / audio) is matched against the filename in the
//! PRD-defined precedence order; the first matching variant wins for that
//! class. Other classes are independent.

use std::sync::LazyLock;

use kino_core::stream::{Audio, Codec, Hdr, ParsedTags, Quality};
use regex::Regex;

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("locked PRD §8 regex must compile")
}

// --- Quality (PRD §8, in spec order) ----------------------------------------

static RE_Q_4K: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(2160p|4K|UHD)\b"));
static RE_Q_1080: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b1080p\b"));
static RE_Q_720: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b720p\b"));
static RE_Q_SD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(480p|576p|DVDRip|SDTV)\b"));

// --- HDR (PRD §8, in spec order) --------------------------------------------

static RE_HDR_DV: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(DV|DoVi|Dolby[. ]Vision)\b"));
static RE_HDR_10PLUS: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bHDR10\+"));
static RE_HDR_10: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bHDR10?\b"));

// --- Codec (PRD §8) ---------------------------------------------------------

static RE_C_AV1: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bAV1\b"));
static RE_C_H265: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(H[. ]?265|HEVC|x265)\b"));
static RE_C_H264: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(H[. ]?264|AVC|x264)\b"));

// --- Audio (PRD §8, in spec order) ------------------------------------------
//
// ADR-029 deviation: PRD §8 specifies trailing `\b` on `EAC3|DDP|DD\+|E-AC-3`
// and `AC3|DD`, but the PRD's own locked fixture `Some Show S01E01 720p WEB-DL
// DDP5.1 H.264` is required to yield EAC3, and `\bDDP\b` cannot match
// `DDP5.1` because there's no word boundary between `P` and `5`. The fixture
// table is the binary behavioral spec for parsing, so we tighten the trailing
// boundary to `(?:\b|\d)`: a word boundary OR a digit (e.g. channel count
// `5.1`/`7.1`). This preserves the original intent — match Dolby Digital
// Plus / AC-3 tokens, ignore false positives like `DDS` — while accepting the
// channel-suffix forms that appear in real scene names. See STATE.md
// "PRD Issues" for the corresponding §8 revision request.

static RE_A_ATMOS: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bAtmos\b"));
static RE_A_TRUEHD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bTrueHD\b"));
static RE_A_DTS_HD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bDTS[-. ]?HD([. ]MA)?\b"));
static RE_A_DTS_X: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bDTS[: -]?X\b"));
static RE_A_EAC3: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)\b(?:EAC3|DDP|DD\+|E-AC-3)(?:\b|\d)"));
static RE_A_AC3: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\b(?:AC3|DD)(?:\b|\d)"));
static RE_A_DTS: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)\bDTS\b"));

/// Detect the [`Quality`] tag for a stream filename, or `None` if no bucket
/// matches. Order follows PRD §8.
#[must_use]
pub fn detect_quality(name: &str) -> Option<Quality> {
    if RE_Q_4K.is_match(name) {
        Some(Quality::Uhd4K)
    } else if RE_Q_1080.is_match(name) {
        Some(Quality::Fhd1080)
    } else if RE_Q_720.is_match(name) {
        Some(Quality::Hd720)
    } else if RE_Q_SD.is_match(name) {
        Some(Quality::Sd)
    } else {
        None
    }
}

/// Detect the [`Hdr`] tag for a stream filename, or `None` if no variant
/// matches. Order follows PRD §8.
#[must_use]
pub fn detect_hdr(name: &str) -> Option<Hdr> {
    if RE_HDR_DV.is_match(name) {
        Some(Hdr::DolbyVision)
    } else if RE_HDR_10PLUS.is_match(name) {
        Some(Hdr::Hdr10Plus)
    } else if RE_HDR_10.is_match(name) {
        Some(Hdr::Hdr10)
    } else {
        None
    }
}

/// Detect the [`Codec`] tag for a stream filename.
#[must_use]
pub fn detect_codec(name: &str) -> Option<Codec> {
    if RE_C_AV1.is_match(name) {
        Some(Codec::Av1)
    } else if RE_C_H265.is_match(name) {
        Some(Codec::H265)
    } else if RE_C_H264.is_match(name) {
        Some(Codec::H264)
    } else {
        None
    }
}

/// Detect the [`Audio`] tag for a stream filename. Order follows PRD §8 —
/// `Atmos`, `TrueHD`, `DTS-HD`, `DTS-X`, `EAC3`, `AC3`, `DTS`.
#[must_use]
pub fn detect_audio(name: &str) -> Option<Audio> {
    if RE_A_ATMOS.is_match(name) {
        Some(Audio::Atmos)
    } else if RE_A_TRUEHD.is_match(name) {
        Some(Audio::TrueHd)
    } else if RE_A_DTS_HD.is_match(name) {
        Some(Audio::DtsHd)
    } else if RE_A_DTS_X.is_match(name) {
        Some(Audio::DtsX)
    } else if RE_A_EAC3.is_match(name) {
        Some(Audio::Eac3)
    } else if RE_A_AC3.is_match(name) {
        Some(Audio::Ac3)
    } else if RE_A_DTS.is_match(name) {
        Some(Audio::Dts)
    } else {
        None
    }
}

/// Convenience: run all four detectors against a single filename.
#[must_use]
pub fn parse(name: &str) -> ParsedTags {
    ParsedTags {
        quality: detect_quality(name),
        hdr: detect_hdr(name),
        codec: detect_codec(name),
        audio: detect_audio(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PRD §8 mandates exactly these four fixtures and exactly these tags.

    #[test]
    fn fixture_matrix_uhd_truehd_atmos() {
        let tags = parse("The Matrix 1999 2160p UHD BluRay HEVC TrueHD Atmos 7.1-FraMeSToR");
        assert_eq!(tags.quality, Some(Quality::Uhd4K));
        assert_eq!(tags.codec, Some(Codec::H265));
        assert_eq!(tags.audio, Some(Audio::Atmos)); // Atmos wins over TrueHD per order.
        assert_eq!(tags.hdr, None);
    }

    #[test]
    fn fixture_inception_dv_hdr10_x265_dtshd() {
        let tags = parse("Inception 2010 1080p BluRay DV HDR10 x265 DTS-HD MA 5.1");
        assert_eq!(tags.quality, Some(Quality::Fhd1080));
        assert_eq!(tags.hdr, Some(Hdr::DolbyVision));
        assert_eq!(tags.codec, Some(Codec::H265));
        assert_eq!(tags.audio, Some(Audio::DtsHd));
    }

    #[test]
    fn fixture_some_show_720p_ddp_h264() {
        let tags = parse("Some Show S01E01 720p WEB-DL DDP5.1 H.264");
        assert_eq!(tags.quality, Some(Quality::Hd720));
        assert_eq!(tags.codec, Some(Codec::H264));
        assert_eq!(tags.audio, Some(Audio::Eac3));
        assert_eq!(tags.hdr, None);
    }

    #[test]
    fn fixture_old_movie_dvdrip() {
        let tags = parse("Old Movie DVDRip XviD");
        assert_eq!(tags.quality, Some(Quality::Sd));
        assert_eq!(tags.codec, None);
        assert_eq!(tags.audio, None);
        assert_eq!(tags.hdr, None);
    }

    #[test]
    fn quality_4k_aliases() {
        assert_eq!(detect_quality("Movie.2160p.mkv"), Some(Quality::Uhd4K));
        assert_eq!(detect_quality("Movie.4K.mkv"), Some(Quality::Uhd4K));
        assert_eq!(detect_quality("Movie.UHD.mkv"), Some(Quality::Uhd4K));
    }

    #[test]
    fn hdr10_plus_does_not_match_as_hdr10() {
        assert_eq!(detect_hdr("Movie HDR10+ x265"), Some(Hdr::Hdr10Plus));
        assert_eq!(detect_hdr("Movie HDR10 x265"), Some(Hdr::Hdr10));
        // PRD §8 regex `\bHDR10?\b` matches `HDR1` or `HDR10`, NOT plain `HDR`.
        // Plain "HDR" in a filename is too ambiguous to confidently tag, so
        // returning None is correct per the locked regex set.
        assert_eq!(detect_hdr("Movie HDR x265"), None);
    }

    #[test]
    fn dv_aliases() {
        assert_eq!(detect_hdr("Movie DoVi 1080p"), Some(Hdr::DolbyVision));
        assert_eq!(
            detect_hdr("Movie Dolby.Vision 1080p"),
            Some(Hdr::DolbyVision)
        );
        assert_eq!(
            detect_hdr("Movie Dolby Vision 1080p"),
            Some(Hdr::DolbyVision)
        );
    }

    #[test]
    fn audio_precedence_atmos_then_truehd() {
        // A name with both Atmos and TrueHD must report Atmos (PRD §8 order).
        assert_eq!(detect_audio("X.TrueHD.Atmos.mkv"), Some(Audio::Atmos));
        assert_eq!(detect_audio("X.TrueHD.mkv"), Some(Audio::TrueHd));
    }

    #[test]
    fn audio_dts_hd_takes_precedence_over_dts() {
        assert_eq!(detect_audio("X.DTS-HD.MA.mkv"), Some(Audio::DtsHd));
        assert_eq!(detect_audio("X.DTS.mkv"), Some(Audio::Dts));
    }

    #[test]
    fn codec_x265_h265_hevc_all_match() {
        assert_eq!(detect_codec("X.x265.mkv"), Some(Codec::H265));
        assert_eq!(detect_codec("X.H265.mkv"), Some(Codec::H265));
        assert_eq!(detect_codec("X.H.265.mkv"), Some(Codec::H265));
        assert_eq!(detect_codec("X.HEVC.mkv"), Some(Codec::H265));
    }

    #[test]
    fn empty_filename_yields_no_tags() {
        let tags = parse("");
        assert_eq!(tags, ParsedTags::default());
    }

    // Regression guard for ADR-029: the `(?:\b|\d)` trailing boundary must
    // still reject codec tokens that bleed into other letters (e.g. `DDS`,
    // `AC3D`-prefixed words, `DDP` inside `DDPL` etc.). Only digit suffixes
    // (channel counts) and real word boundaries should count as a match.
    #[test]
    fn audio_does_not_false_positive_on_letter_suffixes() {
        assert_eq!(detect_audio("Disc DDS Audio"), None);
        assert_eq!(detect_audio("Movie DDPL stuff"), None);
        // Real channel suffix still matches.
        assert_eq!(detect_audio("Movie DDP5.1 stuff"), Some(Audio::Eac3));
        assert_eq!(detect_audio("Movie DD5.1 stuff"), Some(Audio::Ac3));
    }
}
