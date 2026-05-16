//! `kino-core` — shared types and primitives used by every other crate.
//!
//! This crate is intentionally dependency-light. It holds:
//!
//! - Locked numeric constants from PRD §8 (see [`constants`]).
//! - Shared data types ([`title`], [`stream`]) shaped by the PRD.
//! - The crate-level error type [`Error`].
//!
//! Heavier subsystems (DB access, settings loading, install-id management)
//! are wired in by the persistence-layer session (F-002).

pub mod constants;
pub mod stream;
pub mod title;

/// Crate-level error type. Kept narrow on purpose; subsystems add their own
/// thiserror enums and convert when crossing module boundaries.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
