//! `kino-core` — shared types and primitives used by every other crate.
//!
//! This crate holds:
//!
//! - Locked numeric constants from PRD §8 (see [`constants`]).
//! - Shared data types ([`title`], [`stream`], [`cw`], [`addon`]) shaped by
//!   the PRD.
//! - The persistence layer ([`db`]) wired to the workspace-root
//!   `migrations/` directory (PRD §F-002).
//! - The crate-level error type [`Error`].

pub mod addon;
pub mod constants;
pub mod cw;
pub mod db;
pub mod stream;
pub mod title;

pub use db::{Db, DbError, INSTALL_ID_KEY};

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
    #[error(transparent)]
    Db(#[from] DbError),
}

pub type Result<T> = std::result::Result<T, Error>;
