//! Parsed-stream metadata. The tag enums (quality / HDR / audio / codec) are
//! the wire format consumed by `kino-addons::parse` and surfaced in the
//! title-detail stream rows (PRD §F-010).

use serde::{Deserialize, Serialize};

/// Detected video quality bucket (PRD §8 regex set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Quality {
    /// 2160p / 4K / UHD
    #[serde(rename = "4K")]
    Uhd4K,
    #[serde(rename = "1080p")]
    Fhd1080,
    #[serde(rename = "720p")]
    Hd720,
    Sd,
}

/// Detected HDR variant (PRD §8 regex set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Hdr {
    /// Dolby Vision (PRD only commits to profiles 5 and 8.1 in v1; ADR-022).
    #[serde(rename = "DV")]
    DolbyVision,
    #[serde(rename = "HDR10+")]
    Hdr10Plus,
    #[serde(rename = "HDR10")]
    Hdr10,
}

/// Detected video codec (PRD §8 regex set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Codec {
    Av1,
    /// HEVC / H.265 / x265
    H265,
    /// AVC / H.264 / x264
    H264,
}

/// Detected audio format (PRD §8 regex set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Audio {
    Atmos,
    TrueHd,
    DtsHd,
    DtsX,
    Eac3,
    Ac3,
    Dts,
}

/// Output of stream-filename parsing. Mirrors the badges rendered in the
/// title-detail stream row (PRD §F-010).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParsedTags {
    pub quality: Option<Quality>,
    pub hdr: Option<Hdr>,
    pub codec: Option<Codec>,
    pub audio: Option<Audio>,
}
