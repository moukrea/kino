//! F-016 §8 About → Export logs.
//!
//! Two halves:
//!
//! - [`install_file_subscriber`] sets up the `tracing` daily-rotating file
//!   appender that writes to `<app-config>/logs/kino.log[.YYYY-MM-DD]`. The
//!   returned guard MUST outlive the program (Tauri's setup hook owns it),
//!   otherwise the appender's worker thread drops buffered writes on exit.
//!
//! - [`zip_log_dir`] walks the logs directory and produces a `.zip` file at
//!   the user-chosen destination so the support-ticket workflow has a single
//!   shareable artifact.
//!
//! The PRD lists "Export logs button: zips logs folder to a chosen
//! location" under F-016 §8. Files older than the appender's retention are
//! preserved on disk until the directory is cleaned up manually — we never
//! delete files we didn't write.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

/// Sub-directory under the app config root holding rotated log files.
pub const LOG_DIR_NAME: &str = "logs";
/// Stable filename prefix the rolling appender uses. The daily-rotation
/// suffix (`.YYYY-MM-DD`) is appended by `tracing-appender`.
pub const LOG_FILE_PREFIX: &str = "kino.log";

/// Compute the logs directory under the given app config root.
pub fn log_dir(config_root: &Path) -> PathBuf {
    config_root.join(LOG_DIR_NAME)
}

/// Install a daily-rotating file appender for `tracing` and return the
/// `(layer, guard)` pair. The caller is expected to compose `layer` into a
/// `tracing_subscriber` registry and store `guard` somewhere live for the
/// lifetime of the program (e.g. in Tauri's managed state).
///
/// On Linux this writes plain (uncolored) lines so the file is grep-friendly.
/// Returns an `io::Error` if the logs directory can't be created.
pub fn install_file_appender(
    config_root: &Path,
) -> std::io::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    let dir = log_dir(config_root);
    fs::create_dir_all(&dir)?;
    let appender = RollingFileAppender::new(Rotation::DAILY, dir, LOG_FILE_PREFIX);
    Ok(tracing_appender::non_blocking(appender))
}

/// Archive every file currently in the logs directory into a single `.zip`
/// at `dest_zip`. Returns the byte count written. The destination's parent
/// directory must exist; the file itself is created (and truncated on
/// existing match).
///
/// An empty / missing logs dir still produces a valid (but empty) archive
/// so the user gets a recognizable artifact instead of a silent no-op —
/// useful for "the user clicked Export but the daemon hasn't logged
/// anything yet".
pub fn zip_log_dir(log_dir: &Path, dest_zip: &Path) -> std::io::Result<u64> {
    let dest = File::create(dest_zip)?;
    let mut writer = zip::ZipWriter::new(dest);
    let opts: SimpleFileOptions =
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut buf = Vec::with_capacity(64 * 1024);
    if log_dir.exists() {
        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            // Only files at the top level — the rolling appender doesn't
            // create subdirectories.
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            writer
                .start_file(name, opts)
                .map_err(std::io::Error::other)?;
            buf.clear();
            File::open(&path)?.read_to_end(&mut buf)?;
            writer.write_all(&buf)?;
        }
    }
    let f = writer.finish().map_err(std::io::Error::other)?;
    Ok(f.metadata()?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn zip_log_dir_emits_empty_archive_when_logs_absent() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        // `logs` deliberately does not exist.
        let out = dir.path().join("out.zip");
        let bytes = zip_log_dir(&logs, &out).unwrap();
        assert!(out.exists());
        // Empty zip has the central directory end record (~22 bytes); we
        // accept any positive size.
        assert!(bytes > 0, "even an empty archive has the EOCD record");
    }

    #[test]
    fn zip_log_dir_writes_present_files() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let mut f = File::create(logs.join("kino.log.2025-01-01")).unwrap();
        writeln!(f, "hello").unwrap();
        drop(f);
        let out = dir.path().join("out.zip");
        zip_log_dir(&logs, &out).unwrap();

        // Re-read the archive and confirm our entry is present.
        let archive = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        assert!(names.iter().any(|n| n == "kino.log.2025-01-01"));
    }

    #[test]
    fn zip_log_dir_skips_subdirectories() {
        let dir = tempdir().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(logs.join("nested")).unwrap();
        File::create(logs.join("nested").join("ignored.txt")).unwrap();
        File::create(logs.join("kept.log")).unwrap();
        let out = dir.path().join("out.zip");
        zip_log_dir(&logs, &out).unwrap();
        let archive = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        assert!(names.iter().any(|n| n == "kept.log"));
        assert!(
            !names.iter().any(|n| n.contains("ignored.txt")),
            "nested subdirs intentionally skipped"
        );
    }

    #[test]
    fn install_file_appender_creates_logs_dir() {
        let dir = tempdir().unwrap();
        let _ = install_file_appender(dir.path()).unwrap();
        assert!(log_dir(dir.path()).is_dir());
    }
}
