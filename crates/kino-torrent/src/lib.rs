//! `kino-torrent` — embedded torrent engine and adaptive-buffer scheduler.
//!
//! Most of the surface (librqbit session, piece scheduler, HTTP integration
//! with `kino-server`) lands in the F-013 and F-014 sessions. Session 001
//! ships the locked-content modules referenced by PRD §8 so other crates can
//! depend on stable identifiers.

pub mod trackers;
