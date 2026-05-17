//! F-016 §4 Cache controls — filesystem helpers behind the `cache_usage`
//! and `cache_clear` Tauri commands.
//!
//! "Cache" here refers to the on-disk torrent / artwork cache, NOT the
//! application database (which lives next to it but is never wiped from
//! Settings). The directory is resolved via `crate::paths::cache_dir` so
//! the Linux / Android distinction is honored.
//!
//! Both helpers are best-effort:
//! - `dir_size_bytes` walks the tree and sums file sizes, skipping
//!   unreadable entries with a `tracing::warn!` so a single corrupted
//!   sub-tree doesn't make the whole computation fail.
//! - `clear_dir_contents` removes every entry inside the directory but
//!   keeps the directory itself in place so subsequent writes don't have
//!   to recreate it.

use std::fs;
use std::path::Path;

/// Recursively sum the byte size of every regular file under `root`.
/// Returns `0` if `root` does not exist. Symlinks are followed via
/// `Path::metadata` so a librqbit cache that lives behind a symlink still
/// gets accounted for.
pub fn dir_size_bytes(root: &Path) -> u64 {
    if !root.exists() {
        return 0;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "skipping unreadable dir");
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match path.metadata() {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable entry");
                    continue;
                }
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Remove every entry inside `root`, preserving `root` itself. Returns
/// `Ok(())` if `root` does not exist (idempotent). Errors from individual
/// entries propagate so the user gets a real failure message instead of a
/// silent partial wipe.
pub fn clear_dir_contents(root: &Path) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn dir_size_bytes_returns_zero_for_missing_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(dir_size_bytes(&missing), 0);
    }

    #[test]
    fn dir_size_bytes_sums_files_recursively() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("nested")).unwrap();
        let mut a = fs::File::create(dir.path().join("a.bin")).unwrap();
        a.write_all(&[0u8; 1024]).unwrap();
        let mut b = fs::File::create(dir.path().join("nested/b.bin")).unwrap();
        b.write_all(&[0u8; 4096]).unwrap();
        drop(a);
        drop(b);
        assert_eq!(dir_size_bytes(dir.path()), 5120);
    }

    #[test]
    fn clear_dir_contents_keeps_root_and_removes_entries() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("nested")).unwrap();
        fs::File::create(dir.path().join("a.bin")).unwrap();
        fs::File::create(dir.path().join("nested/b.bin")).unwrap();
        clear_dir_contents(dir.path()).unwrap();
        assert!(dir.path().is_dir());
        let remaining: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert!(remaining.is_empty());
    }

    #[test]
    fn clear_dir_contents_idempotent_on_missing_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");
        clear_dir_contents(&missing).unwrap();
    }
}
