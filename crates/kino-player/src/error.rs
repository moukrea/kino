//! Error types for the kino player layer (PRD §F-015).

use std::io;

/// Errors returned by the player drivers and the IPC layer beneath them.
///
/// `Spawn` / `Io` / `IpcWrite` / `IpcRead` are wire / process errors —
/// the player process couldn't be launched, or the JSON-IPC socket
/// closed unexpectedly. `Backend` carries an error message reported by
/// the player itself (mpv `error` field in a response payload). `Parse`
/// is the IPC line parser failing — guards against the player printing
/// a non-JSON line.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    /// The backend process could not be started. Most common cause on
    /// Linux: `mpv` is not installed.
    #[error("failed to spawn player process: {0}")]
    Spawn(#[source] io::Error),
    /// Generic I/O failure on the IPC socket or stdout/stderr pipes.
    #[error("player IPC io: {0}")]
    Io(#[source] io::Error),
    /// The player process closed before the command was acknowledged.
    /// Includes any captured stderr tail so log readers see the cause.
    #[error("player process exited prematurely: {0}")]
    Closed(String),
    /// Sending a command across the IPC socket failed.
    #[error("failed to write IPC frame: {0}")]
    IpcWrite(#[source] io::Error),
    /// Reading from the IPC socket failed.
    #[error("failed to read IPC frame: {0}")]
    IpcRead(#[source] io::Error),
    /// The remote player reported a backend-level error for a command —
    /// the wire-level transport succeeded but the operation didn't.
    /// Mpv example: `{"error": "property unavailable"}`.
    #[error("player backend error: {0}")]
    Backend(String),
    /// JSON parse failure on a received IPC line. Carries the offending
    /// text up to a reasonable length for log readers.
    #[error("failed to parse IPC frame: {message} (line: {line})")]
    Parse { message: String, line: String },
    /// Caller asked for an operation that requires an open file but
    /// nothing is currently loaded (e.g. `seek` before `open`).
    #[error("no media loaded")]
    NoMedia,
    /// Player driver is busy with another open/close transition.
    #[error("player driver busy: {0}")]
    Busy(String),
}

impl PlayerError {
    /// Helper: wrap an I/O error encountered while reading from the IPC
    /// socket.
    pub fn read(e: io::Error) -> Self {
        Self::IpcRead(e)
    }

    /// Helper: wrap an I/O error encountered while writing to the IPC
    /// socket.
    pub fn write(e: io::Error) -> Self {
        Self::IpcWrite(e)
    }
}
