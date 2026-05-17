//! Crate-level error type for `kino-metadata`.
//!
//! Each provider client returns this. The Tauri host converts to `String`
//! at the IPC boundary per ADR-039.
//!
//! HTTP-layer errors flow in from `kino_core::http::HttpError` via the
//! `From` impl below; the variants here keep their existing shape so the
//! lift from `kino-metadata::http` to `kino-core::http` (Session 008,
//! ADR-055) is invisible to downstream callers.

use kino_core::http::HttpError;

/// Errors that can surface from any metadata client call.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying transport failure (connect, timeout, TLS, request build).
    /// The retry policy converts transient instances of this into a final
    /// `Network` only after the backoff schedule is exhausted.
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),

    /// The remote returned a non-2xx status that the retry policy did not
    /// recover from. `body` is the response body trimmed to a sensible size.
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },

    /// Response body did not match the expected shape. Carries a short
    /// diagnostic. Reserved for the per-provider parsers added in F-004+.
    #[error("decode: {0}")]
    Decode(String),

    /// The configured `kino_core::Db` is missing the API key the caller
    /// asked for. `provider` is the human-readable provider name.
    #[error("missing API key: {provider}")]
    MissingKey { provider: &'static str },
}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Network(r) => Self::Network(r),
            HttpError::Http { status, body } => Self::Http { status, body },
        }
    }
}
