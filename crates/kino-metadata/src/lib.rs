//! `kino-metadata` — HTTP clients for TMDB, Trakt, TVDB, and Fanart.tv
//! (PRD §F-003).
//!
//! Each provider lives in its own module under `crates/kino-metadata/src/`.
//! Per-provider clients share the [`http::HttpConfig`] / [`http::fetch_with_retry`]
//! primitives so the locked retry policy (3 retries, backoff `[1s, 2s, 4s]` on
//! 5xx / 429 / network errors per PRD §8) and the locked User-Agent string
//! (PRD §F-003) are honored uniformly.
//!
//! ## Settings keys
//!
//! API keys live in the `settings` table under the locked keys exposed below
//! ([`TMDB_API_KEY`], [`TRAKT_API_KEY`], [`TVDB_API_KEY`], [`FANART_API_KEY`]).
//! The app ships with no keys; the host commands return an error pointing at
//! the missing key when one isn't configured. The setup wizard (F-016) is
//! responsible for asking the user to fill these in.

pub mod artwork;
pub mod error;
pub mod fanart;
pub mod http;
pub mod tmdb;
pub mod trakt;
pub mod trending;
pub mod tvdb;

pub use artwork::{
    fetch_and_resolve, lang_chain_hash, resolve, ArtField, ArtKind, Artwork, LangAsset, LangText,
    ProviderArtBundle, SummaryField, TitleIds,
};
pub use error::Error;
pub use fanart::FanartClient;
pub use http::{HttpConfig, USER_AGENT};
pub use tmdb::TmdbClient;
pub use trakt::TraktClient;
pub use trending::{aggregate, ProviderItem};
pub use tvdb::TvdbClient;

/// `settings.key` storing the TMDB API key (PRD §F-003).
pub const TMDB_API_KEY: &str = "tmdb_api_key";

/// `settings.key` storing the Trakt application API key (PRD §F-003).
pub const TRAKT_API_KEY: &str = "trakt_api_key";

/// `settings.key` storing the TVDB v4 API key (PRD §F-003).
pub const TVDB_API_KEY: &str = "tvdb_api_key";

/// `settings.key` storing the Fanart.tv API key (PRD §F-003).
pub const FANART_API_KEY: &str = "fanart_api_key";
