//! Engine: thin async wrapper around [`librqbit::Session`].
//!
//! ## Configuration (PRD §F-013 locked)
//!
//! - **Cache root.** [`EngineConfig::cache_root`] — both the
//!   `default_output_folder` for `librqbit::Session::new_with_opts` AND the
//!   persistence directory for resume state. Honors the user's setting
//!   (`cache.path`) per F-016 §4.
//! - **DHT / PEX / LSD.** Enabled by default. Toggled off only for tests
//!   that don't want a listener.
//! - **Supplementary trackers.** Pre-seeded from [`crate::trackers`].
//! - **Listen port.** OS-assigned (PRD §F-013 "Port: OS-assigned"); we leave
//!   `listen_port_range` `None`, which lets librqbit pick.
//! - **Persistence.** Disabled in this session. Resume state across app
//!   restarts is an F-018 polish item; for v1 we re-prefetch metadata.
//!
//! ## What this module deliberately does NOT do
//!
//! - **No piece-priority scheduler.** That's F-014's job. Until then,
//!   librqbit's default sequential/random scheduling drives playback.
//! - **No cache eviction.** librqbit's storage layer manages on-disk
//!   pieces; explicit LRU lives in F-014 as well.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncSeek};
use url::Url;

/// Object-safe combination of `AsyncRead + AsyncSeek` returned by
/// [`AddedTorrent::open_stream`]. Blanket-impl'd for any type that satisfies
/// both bounds — currently the librqbit `FileStream`. Exposed as a `dyn`
/// trait object so consumers (`kino-server`) can box a stream without
/// naming the (private) inner type.
pub trait FileStream: AsyncRead + AsyncSeek + Send + Unpin {}
impl<T: AsyncRead + AsyncSeek + Send + Unpin + ?Sized> FileStream for T {}

use crate::trackers::SUPPLEMENTARY_TRACKERS;

/// Video file extensions used by [`AddedTorrent::pick_largest_video`] when
/// the torrent is multi-file (e.g. a season pack). Order does not affect
/// selection — the largest file matching ANY of these extensions wins.
pub const LARGEST_VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "webm", "ts", "mpg", "mpeg", "wmv", "flv",
];

/// Lock-stepped knobs taken from PRD §F-013 / §8. The Tauri host fills this
/// in at startup; tests build a relaxed config (DHT disabled, alt cache dir).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Filesystem root for downloaded torrent data. Maps to
    /// `Session::new_with_opts(default_output_folder = …)`. Per-add overrides
    /// are not exposed from kino — every torrent lands under this root.
    pub cache_root: PathBuf,
    /// PRD §F-013 "DHT enabled". Set `false` only to make tests offline.
    pub enable_dht: bool,
    /// PRD §F-013 "PEX enabled". librqbit folds PEX/LSD into the same
    /// peer-discovery layer; this toggle is reserved for future use.
    pub enable_pex: bool,
    /// PRD §F-013 "LSD enabled". Same caveat as `enable_pex`.
    pub enable_lsd: bool,
    /// Supplementary trackers appended to every added torrent (PRD §8).
    pub supplementary_trackers: Vec<String>,
    /// Initial timeout for `wait_until_initialized`. PRD acceptance says a
    /// magnet should yield a streaming URL within 5 s; we give ourselves
    /// `2 ×` for tracker round-trips.
    pub init_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            cache_root: PathBuf::from("."),
            enable_dht: true,
            enable_pex: true,
            enable_lsd: true,
            supplementary_trackers: SUPPLEMENTARY_TRACKERS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            init_timeout: Duration::from_secs(10),
        }
    }
}

/// Errors raised by the engine. `Internal` wraps `anyhow::Error` to bridge
/// librqbit's `anyhow::Result` surface; the `Display` impl deliberately
/// surfaces the chain.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("invalid magnet or torrent: {0}")]
    InvalidInput(String),
    #[error("torrent metadata not received within {0:?}")]
    InitTimeout(Duration),
    #[error("no playable file found in the torrent")]
    NoPlayableFile,
    #[error("requested file index {requested} is out of range (have {file_count})")]
    FileIndexOutOfRange { requested: usize, file_count: usize },
    #[error("torrent {0} is not currently managed by this engine")]
    UnknownTorrent(usize),
    #[error("session i/o: {0}")]
    Internal(#[from] anyhow::Error),
}

pub type Result<T, E = EngineError> = std::result::Result<T, E>;

/// Inputs accepted by [`Engine::add`].
#[derive(Debug, Clone)]
pub enum AddInput {
    /// `magnet:?xt=urn:btih:…` URI (also accepts a plain `http(s)` link to
    /// a `.torrent` file — librqbit's `AddTorrent::from_url` handles both).
    Url(String),
    /// Raw `.torrent` metainfo bytes (e.g. uploaded by the user or fetched
    /// by an addon).
    Bytes(Bytes),
}

/// File-level details surfaced after a torrent is added.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileInfo {
    /// Index into [`AddedTorrent::files`]; pass to
    /// [`AddedTorrent::open_stream`] to read bytes.
    pub index: usize,
    /// Path relative to the torrent root. Forward-slash on every host.
    pub relative_path: String,
    /// Size in bytes.
    pub size: u64,
}

impl FileInfo {
    /// `true` iff the lowercase file extension is in
    /// [`LARGEST_VIDEO_EXTENSIONS`].
    #[must_use]
    pub fn is_video(&self) -> bool {
        Path::new(&self.relative_path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                let ext_lc = ext.to_ascii_lowercase();
                LARGEST_VIDEO_EXTENSIONS.iter().any(|e| **e == ext_lc)
            })
    }
}

/// Handle returned from [`Engine::add`]. Holds the underlying
/// `Arc<ManagedTorrent>` plus a derived file list so callers don't need to
/// re-walk metadata for every UI tick. Cheaply [`Clone`] — every field is
/// already shared via `Arc`.
#[derive(Clone)]
pub struct AddedTorrent {
    inner: Arc<ManagedTorrent>,
    inner_files: Arc<Vec<FileInfo>>,
    name: Arc<Option<String>>,
    info_hash_hex: Arc<String>,
}

impl std::fmt::Debug for AddedTorrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddedTorrent")
            .field("id", &self.inner.id())
            .field("name", &*self.name)
            .field("info_hash", &*self.info_hash_hex)
            .field("files", &self.inner_files.len())
            .finish()
    }
}

impl AddedTorrent {
    /// Stable librqbit-assigned id; the engine uses this to address the
    /// torrent for removal.
    #[must_use]
    pub fn id(&self) -> usize {
        self.inner.id()
    }

    /// Display name (the torrent's name field or the largest file's basename
    /// fallback). `None` only if librqbit couldn't decode the metainfo, which
    /// shouldn't happen after `wait_until_initialized` succeeds.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Lower-hex info hash. Useful for logging and as a stable id across
    /// process restarts.
    #[must_use]
    pub fn info_hash_hex(&self) -> &str {
        self.info_hash_hex.as_str()
    }

    /// All files contained in the torrent, in the order librqbit reports
    /// them.
    #[must_use]
    pub fn files(&self) -> &[FileInfo] {
        &self.inner_files
    }

    /// Pick the largest file whose extension is in
    /// [`LARGEST_VIDEO_EXTENSIONS`]. If no file has a video extension —
    /// possible with mislabelled torrents — fall back to the largest file
    /// overall. Returns `None` only if the torrent is empty.
    #[must_use]
    pub fn pick_largest_video(&self) -> Option<&FileInfo> {
        let video = self
            .inner_files
            .iter()
            .filter(|f| f.is_video())
            .max_by_key(|f| f.size);
        if video.is_some() {
            return video;
        }
        self.inner_files.iter().max_by_key(|f| f.size)
    }

    /// Open a streaming read handle to the given file. Returns
    /// [`EngineError::FileIndexOutOfRange`] for invalid indices and
    /// [`EngineError::Internal`] if librqbit refuses (typically because the
    /// torrent isn't initialized yet).
    ///
    /// The returned value implements [`tokio::io::AsyncRead`] +
    /// [`tokio::io::AsyncSeek`]. Each call returns a fresh stream — librqbit
    /// uses an internal semaphore to bound concurrent reads. Holding the
    /// returned value pins one slot until it's dropped.
    pub fn open_stream(&self, file_index: usize) -> Result<Box<dyn FileStream>> {
        if file_index >= self.inner_files.len() {
            return Err(EngineError::FileIndexOutOfRange {
                requested: file_index,
                file_count: self.inner_files.len(),
            });
        }
        let s = self
            .inner
            .clone()
            .stream(file_index)
            .map_err(EngineError::Internal)?;
        Ok(Box::new(s))
    }

    /// File length of the file at `file_index`, in bytes. Equivalent to
    /// `files()[i].size` but spelled as a method for ergonomics in the
    /// `kino-server` route handlers.
    #[must_use]
    pub fn file_size(&self, file_index: usize) -> Option<u64> {
        self.inner_files.get(file_index).map(|f| f.size)
    }

    /// File path of the file at `file_index`, relative to the torrent root.
    #[must_use]
    pub fn file_name(&self, file_index: usize) -> Option<&str> {
        self.inner_files
            .get(file_index)
            .map(|f| f.relative_path.as_str())
    }
}

/// kino's torrent engine: wraps a single `librqbit::Session`.
///
/// Cheap to clone (`Arc` internally); designed to be stored once in Tauri's
/// managed state.
#[derive(Clone)]
pub struct Engine {
    session: Arc<Session>,
    config: EngineConfig,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("cache_root", &self.config.cache_root)
            .field("dht", &self.config.enable_dht)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Build a new engine. `cache_root` is created (recursively) if it
    /// doesn't already exist.
    pub async fn new(config: EngineConfig) -> Result<Self> {
        tokio::fs::create_dir_all(&config.cache_root)
            .await
            .with_context(|| {
                format!(
                    "failed to create torrent cache root {}",
                    config.cache_root.display()
                )
            })?;

        let trackers: HashSet<Url> = config
            .supplementary_trackers
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .collect();

        let opts = SessionOptions {
            disable_dht: !config.enable_dht,
            // Reusing DHT routing state across runs is irrelevant for a
            // streaming app — the v1 launch experience is identical every
            // time. Persistence stays off to avoid leaking long-lived state
            // through the cache dir (the user toggles cache via Settings).
            disable_dht_persistence: true,
            trackers,
            ..Default::default()
        };

        let session = Session::new_with_opts(config.cache_root.clone(), opts)
            .await
            .map_err(EngineError::Internal)?;

        Ok(Self { session, config })
    }

    /// Add a magnet or torrent-bytes input. Awaits `wait_until_initialized`
    /// up to [`EngineConfig::init_timeout`] so the caller can assume file
    /// metadata is present on success.
    pub async fn add(&self, input: AddInput) -> Result<AddedTorrent> {
        let add = match input {
            AddInput::Url(url) => {
                if url.trim().is_empty() {
                    return Err(EngineError::InvalidInput("empty url".into()));
                }
                AddTorrent::from_url(url)
            }
            AddInput::Bytes(b) => {
                if b.is_empty() {
                    return Err(EngineError::InvalidInput("empty torrent bytes".into()));
                }
                AddTorrent::from_bytes(b)
            }
        };

        let opts = AddTorrentOptions {
            overwrite: true,
            ..Default::default()
        };

        let response = self
            .session
            .add_torrent(add, Some(opts))
            .await
            .map_err(EngineError::Internal)?;

        let handle = match response {
            AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
            AddTorrentResponse::ListOnly(_) => {
                return Err(EngineError::Internal(anyhow::anyhow!(
                    "add_torrent returned ListOnly despite list_only=false"
                )))
            }
        };

        tokio::time::timeout(self.config.init_timeout, handle.wait_until_initialized())
            .await
            .map_err(|_| EngineError::InitTimeout(self.config.init_timeout))?
            .map_err(EngineError::Internal)?;

        let (files, name, info_hash_hex) = handle
            .with_metadata(|m| {
                let files = m
                    .file_infos
                    .iter()
                    .enumerate()
                    .map(|(index, fi)| FileInfo {
                        index,
                        relative_path: fi
                            .relative_filename
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/"),
                        size: fi.len,
                    })
                    .collect::<Vec<_>>();
                let name = m.name.clone();
                let info_hash_hex = hex::encode(handle.info_hash().0);
                (files, name, info_hash_hex)
            })
            .map_err(EngineError::Internal)?;

        if files.is_empty() {
            return Err(EngineError::NoPlayableFile);
        }

        Ok(AddedTorrent {
            inner: handle,
            inner_files: Arc::new(files),
            name: Arc::new(name),
            info_hash_hex: Arc::new(info_hash_hex),
        })
    }

    /// Remove a torrent previously returned by [`Engine::add`]. When
    /// `delete_files` is `true`, librqbit also wipes the on-disk pieces.
    pub async fn remove(&self, torrent_id: usize, delete_files: bool) -> Result<()> {
        self.session
            .delete(torrent_id.into(), delete_files)
            .await
            .map_err(EngineError::Internal)
    }

    /// Cache root currently in use (resolved at construction time).
    #[must_use]
    pub fn cache_root(&self) -> &Path {
        &self.config.cache_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_info_is_video_recognizes_common_extensions() {
        for ext in ["mkv", "MP4", "m4v", "Avi", "webm", "TS"] {
            let f = FileInfo {
                index: 0,
                relative_path: format!("Movie.2024.{ext}"),
                size: 100,
            };
            assert!(f.is_video(), "expected {ext} to be recognized as video");
        }
        let txt = FileInfo {
            index: 0,
            relative_path: "RARBG.txt".into(),
            size: 100,
        };
        assert!(!txt.is_video());
    }

    #[test]
    fn engine_config_defaults_match_prd() {
        let c = EngineConfig::default();
        assert!(c.enable_dht);
        assert!(c.enable_pex);
        assert!(c.enable_lsd);
        // PRD §8 supplementary trackers ship with the app.
        assert_eq!(c.supplementary_trackers.len(), SUPPLEMENTARY_TRACKERS.len());
    }

    #[test]
    fn pick_largest_video_prefers_video_extensions() {
        let inner = added_torrent_with_files(vec![
            ("RARBG.txt", 1024),
            ("Movie.2024.1080p.mkv", 10_000),
            ("Movie.2024.sample.mkv", 100),
            ("subs/eng.srt", 50),
        ]);
        let picked = inner.pick_largest_video().expect("at least one video");
        assert_eq!(picked.relative_path, "Movie.2024.1080p.mkv");
    }

    #[test]
    fn pick_largest_video_falls_back_to_largest_when_no_video_extension() {
        let inner = added_torrent_with_files(vec![("doc.pdf", 50_000), ("cover.jpg", 100)]);
        let picked = inner.pick_largest_video().expect("at least one file");
        assert_eq!(picked.relative_path, "doc.pdf");
    }

    #[test]
    fn pick_largest_video_returns_none_for_empty() {
        let inner = added_torrent_with_files(vec![]);
        assert!(inner.pick_largest_video().is_none());
    }

    fn added_torrent_with_files(files: Vec<(&str, u64)>) -> AddedTorrentStub {
        let files = files
            .into_iter()
            .enumerate()
            .map(|(index, (path, size))| FileInfo {
                index,
                relative_path: path.to_string(),
                size,
            })
            .collect();
        AddedTorrentStub { files }
    }

    // Test-only helper to exercise the pure file-selection logic without a
    // live librqbit session. Mirrors the methods we care about on
    // AddedTorrent.
    struct AddedTorrentStub {
        files: Vec<FileInfo>,
    }
    impl AddedTorrentStub {
        fn pick_largest_video(&self) -> Option<&FileInfo> {
            let video = self
                .files
                .iter()
                .filter(|f| f.is_video())
                .max_by_key(|f| f.size);
            if video.is_some() {
                return video;
            }
            self.files.iter().max_by_key(|f| f.size)
        }
    }
}
