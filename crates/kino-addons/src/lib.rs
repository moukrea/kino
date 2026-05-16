//! `kino-addons` — Stremio addon protocol client.
//!
//! Session 001 ships only the locked subsystems referenced by PRD §8:
//! [`parse`] (filename → tags regex set) and [`recommended`] (default addon
//! catalog). The full protocol client (`install_addon`, `list_addons`,
//! manifest validation, etc.) lands in the F-007 session.

pub mod parse;
pub mod recommended;
