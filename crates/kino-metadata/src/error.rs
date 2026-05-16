//! Crate-level error type for `kino-metadata`.
//!
//! Each provider client returns this. The Tauri host converts to `String`
//! at the IPC boundary per ADR-039.

use reqwest::StatusCode;

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

impl Error {
    /// Build an [`Error::Http`] from a status code and response body.
    pub(crate) fn http_status(status: StatusCode, body: String) -> Self {
        const MAX_BODY: usize = 512;
        let mut body = body;
        if body.len() > MAX_BODY {
            // Truncate on a char boundary; `floor_char_boundary` is unstable,
            // so walk back until we hit one.
            let mut end = MAX_BODY;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            body.push('…');
        }
        Self::Http {
            status: status.as_u16(),
            body,
        }
    }
}
