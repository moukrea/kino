//! `kino-metadata` — HTTP clients for TMDB, Trakt, TVDB, and Fanart.tv.
//!
//! The per-provider clients (with retry, `ETag`, and TTL caching) land in the
//! F-003 session. This shell exists so other crates can already depend on
//! `kino-metadata` by path.
