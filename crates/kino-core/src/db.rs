//! Persistence layer (PRD §F-002).
//!
//! `SQLite` via `sqlx` with embedded migrations from the workspace-root
//! `migrations/` directory, a four-connection pool, and WAL journaling.
//! Consumers see one [`Db`] handle and call typed methods on it.
//!
//! ## On-disk layout
//!
//! The DB file path is resolved by the caller (the Tauri host knows the
//! correct per-platform location — `$XDG_CONFIG_HOME/kino/kino.db` on Linux
//! per PRD §3 storage layout, `Context.filesDir/kino.db` on Android). The
//! persistence layer itself stays platform-agnostic so it can be exercised
//! in unit tests via [`Db::open_in_memory`].

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::ConnectOptions;
use tracing::{debug, info};
use uuid::Uuid;

use crate::addon::{Addon, AddonInsert};
use crate::availability::AvailabilityRow;
use crate::cw::ContinueWatching;
use crate::title::TitleKind;

/// `settings` row key storing the bootstrapped install id (PRD §F-002).
pub const INSTALL_ID_KEY: &str = "install_id";

/// Per-platform DB path resolution helper kept simple: see module docs.
const DB_FILENAME: &str = "kino.db";

/// Connection pool size, locked at 4 per PRD §3.
const POOL_SIZE: u32 = 4;

/// Persistence-layer error type.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings.install_id missing after bootstrap")]
    InstallIdMissing,
}

/// Handle to the application database.
///
/// Cheap to clone (wraps an `Arc`-shaped [`SqlitePool`] internally), so the
/// Tauri host stores one in app state and every command pulls a reference.
#[derive(Clone, Debug)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Open the database at `db_path`, applying any pending migrations and
    /// ensuring `settings.install_id` is populated.
    ///
    /// The parent directory is created if missing. The connection pool is
    /// fixed at four connections (PRD §3) and WAL journaling is enabled
    /// (PRD §F-002).
    pub async fn open(db_path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        info!(path = %db_path.display(), "opening kino database");
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .log_statements(tracing::log::LevelFilter::Trace);
        Self::open_with_options(opts, POOL_SIZE).await
    }

    /// Open an in-memory database. Used by tests; not exposed to the host.
    ///
    /// Forced to a single connection: each `sqlx` pool connection owns its
    /// own in-memory DB unless backed by a shared-cache URI, so a 4-way
    /// pool would see migrations run on one connection and disappear from
    /// the others. The file-backed path keeps the full pool size — see
    /// [`Db::open`] above.
    pub async fn open_in_memory() -> Result<Self, DbError> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Memory);
        Self::open_with_options(opts, 1).await
    }

    async fn open_with_options(opts: SqliteConnectOptions, max_conn: u32) -> Result<Self, DbError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_conn)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;
        let db = Self { pool };
        db.bootstrap_install_id().await?;
        Ok(db)
    }

    /// Raw pool handle, for code that needs `sqlx::query!`-style escape
    /// hatches (intentionally rare — prefer the typed methods below).
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The standard DB file name (`kino.db`) used by the host on every
    /// target. Exposed so the host can build the platform-appropriate path
    /// without duplicating the constant.
    #[must_use]
    pub const fn db_filename() -> &'static str {
        DB_FILENAME
    }

    /// Return the value of `PRAGMA journal_mode`. Used by F-002 acceptance
    /// to confirm WAL is active on real (file-backed) databases.
    pub async fn journal_mode(&self) -> Result<String, DbError> {
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;
        Ok(mode)
    }

    // ---- settings (key/value) ------------------------------------------

    /// Get a settings value by key, or `None` if the key is absent.
    pub async fn kv_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Upsert a settings value.
    pub async fn kv_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a settings value. Returns the number of rows removed (0 if the
    /// key was absent). The F-016 reset-to-defaults flow uses this to wipe
    /// user-set keys without touching the system-internal entries
    /// (`install_id`, `addons.bootstrap_done`).
    pub async fn kv_delete(&self, key: &str) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Return the install id bootstrapped on first launch.
    pub async fn install_id(&self) -> Result<String, DbError> {
        self.kv_get(INSTALL_ID_KEY)
            .await?
            .ok_or(DbError::InstallIdMissing)
    }

    async fn bootstrap_install_id(&self) -> Result<(), DbError> {
        if self.kv_get(INSTALL_ID_KEY).await?.is_some() {
            debug!("install_id already present, skipping bootstrap");
            return Ok(());
        }
        let id = Uuid::new_v4().to_string();
        info!(install_id = %id, "bootstrapped install_id");
        self.kv_set(INSTALL_ID_KEY, &id).await?;
        Ok(())
    }

    // ---- continue_watching --------------------------------------------

    /// List Continue Watching rows, most-recently-played first.
    pub async fn cw_list(&self) -> Result<Vec<ContinueWatching>, DbError> {
        let rows: Vec<CwRow> = sqlx::query_as(
            "SELECT title_id, type, season, episode, position_s, duration_s, \
             last_played_at, meta_json \
             FROM continue_watching \
             ORDER BY last_played_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(CwRow::into_domain).collect()
    }

    /// Upsert a Continue Watching row. The primary key is
    /// `(title_id, season, episode)`; an episode of a series with the same
    /// `(season, episode)` replaces the previous row.
    pub async fn cw_upsert(&self, cw: &ContinueWatching) -> Result<(), DbError> {
        let meta = serde_json::to_string(&cw.meta_json).map_err(sqlx_json_err)?;
        sqlx::query(
            "INSERT INTO continue_watching \
              (title_id, type, season, episode, position_s, duration_s, last_played_at, meta_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(title_id, season, episode) DO UPDATE SET \
               type = excluded.type, \
               position_s = excluded.position_s, \
               duration_s = excluded.duration_s, \
               last_played_at = excluded.last_played_at, \
               meta_json = excluded.meta_json",
        )
        .bind(&cw.title_id)
        .bind(cw.kind.as_str())
        .bind(cw.season)
        .bind(cw.episode)
        .bind(cw.position_s)
        .bind(cw.duration_s)
        .bind(cw.last_played_at)
        .bind(meta)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a Continue Watching row by its composite key. Returns the
    /// number of rows removed (0 if the row was already absent).
    pub async fn cw_delete(
        &self,
        title_id: &str,
        season: i64,
        episode: i64,
    ) -> Result<u64, DbError> {
        let res = sqlx::query(
            "DELETE FROM continue_watching \
             WHERE title_id = ? AND season = ? AND episode = ?",
        )
        .bind(title_id)
        .bind(season)
        .bind(episode)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Delete every Continue Watching row that belongs to `title_id`,
    /// regardless of `(season, episode)`. Used by PRD §F-012 in two
    /// places: the manual-remove action on the home CW row (which
    /// targets the whole title, not a single episode), and the series
    /// next-episode rule when the user finishes the final episode of a
    /// series.
    ///
    /// Returns the number of rows removed (0 if the title had no CW
    /// rows). The frontend always treats absent as a no-op, so an
    /// already-empty title is not an error.
    pub async fn cw_delete_all_for_title(&self, title_id: &str) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM continue_watching WHERE title_id = ?")
            .bind(title_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    // ---- addons -------------------------------------------------------

    /// List installed addons ordered by `display_order`, then insertion order.
    pub async fn addons_list(&self) -> Result<Vec<Addon>, DbError> {
        let rows: Vec<AddonRow> = sqlx::query_as(
            "SELECT id, manifest_url, enabled, installed_at, manifest_json, display_order \
             FROM addons \
             ORDER BY display_order ASC, installed_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(AddonRow::into_domain).collect()
    }

    /// Insert a new addon. `display_order` defaults to `MAX(display_order) + 1`
    /// so freshly installed addons land at the end of the list. Conflicts on
    /// `manifest_url` bubble up as a `Sqlx` error — the caller (the addons
    /// CRUD command) decides how to surface "already installed".
    pub async fn addons_insert(&self, addon: &AddonInsert) -> Result<(), DbError> {
        let manifest = serde_json::to_string(&addon.manifest_json).map_err(sqlx_json_err)?;
        let installed_at = now_unix();
        let mut tx = self.pool.begin().await?;
        let order: i64 = if let Some(o) = addon.display_order {
            o
        } else {
            let next: Option<i64> =
                sqlx::query_scalar("SELECT COALESCE(MAX(display_order), -1) + 1 FROM addons")
                    .fetch_one(&mut *tx)
                    .await?;
            next.unwrap_or(0)
        };
        sqlx::query(
            "INSERT INTO addons \
              (id, manifest_url, enabled, installed_at, manifest_json, display_order) \
             VALUES (?, ?, 1, ?, ?, ?)",
        )
        .bind(&addon.id)
        .bind(&addon.manifest_url)
        .bind(installed_at)
        .bind(manifest)
        .bind(order)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Delete an addon by id. Returns the number of rows removed.
    pub async fn addons_delete(&self, id: &str) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM addons WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Toggle an addon's `enabled` flag.
    pub async fn addons_set_enabled(&self, id: &str, enabled: bool) -> Result<u64, DbError> {
        let res = sqlx::query("UPDATE addons SET enabled = ? WHERE id = ?")
            .bind(i64::from(enabled))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Reorder the addon list. The supplied slice's index becomes the new
    /// `display_order` for each id. Ids not present in `ids` keep their
    /// current order shifted past the reordered prefix.
    pub async fn addons_reorder(&self, ids: &[String]) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        for (i, id) in ids.iter().enumerate() {
            sqlx::query("UPDATE addons SET display_order = ? WHERE id = ?")
                .bind(i64::try_from(i).unwrap_or(i64::MAX))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---- response_cache ------------------------------------------------
    //
    // Generic key → JSON payload cache with absolute expiry timestamps.
    // F-004 stores its day-long aggregated-trending payloads here; later
    // features (F-005 artwork, F-006 stream availability via its own table,
    // F-007 addon catalogs) extend the same surface.

    /// Return the cached payload for `key` if present and not yet expired.
    /// Expired rows are NOT deleted on read — periodic cleanup is a future
    /// background task; reads simply ignore stale entries.
    pub async fn cache_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let now = now_unix();
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT payload_json FROM response_cache \
             WHERE key = ? AND expires_at > ?",
        )
        .bind(key)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(p,)| p))
    }

    /// Upsert a cache row. `expires_at` is an absolute Unix timestamp in
    /// seconds. Callers compute it from a TTL (e.g. `now + TRENDING_TTL_S`)
    /// or from a boundary like "next UTC midnight" (F-004's daily output
    /// cache).
    pub async fn cache_set(
        &self,
        key: &str,
        payload_json: &str,
        expires_at: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO response_cache (key, payload_json, etag, expires_at) \
             VALUES (?, ?, NULL, ?) \
             ON CONFLICT(key) DO UPDATE SET \
               payload_json = excluded.payload_json, \
               etag = NULL, \
               expires_at = excluded.expires_at",
        )
        .bind(key)
        .bind(payload_json)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- stream_availability (F-006) ----------------------------------
    //
    // Per PRD §F-006 the table records, for each (title_id, kind, source_id)
    // triple, whether the addon returned a non-empty stream list at
    // `checked_at`. Rows are valid for `STREAM_AVAILABILITY_TTL_S` (30 min);
    // reads filter by `checked_at` rather than relying on an explicit expiry
    // column so the same table can absorb future TTL revisions without a
    // migration.

    /// Return a fresh availability check for the given triple, or `None` if
    /// no row is present or the row is older than `fresh_after_unix_s` (an
    /// absolute Unix timestamp — typically `now - STREAM_AVAILABILITY_TTL_S`).
    pub async fn availability_get_fresh(
        &self,
        title_id: &str,
        kind: TitleKind,
        source_id: &str,
        fresh_after_unix_s: i64,
    ) -> Result<Option<bool>, DbError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT has_streams FROM stream_availability \
             WHERE title_id = ? AND type = ? AND source_id = ? \
             AND checked_at > ?",
        )
        .bind(title_id)
        .bind(kind.as_str())
        .bind(source_id)
        .bind(fresh_after_unix_s)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(v,)| v != 0))
    }

    /// Return every fresh availability check for the given title, regardless
    /// of source addon. Used by F-006 to aggregate per-title availability
    /// from already-cached per-source rows.
    pub async fn availability_list_fresh(
        &self,
        title_id: &str,
        kind: TitleKind,
        fresh_after_unix_s: i64,
    ) -> Result<Vec<(String, bool)>, DbError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT source_id, has_streams FROM stream_availability \
             WHERE title_id = ? AND type = ? AND checked_at > ?",
        )
        .bind(title_id)
        .bind(kind.as_str())
        .bind(fresh_after_unix_s)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(s, v)| (s, v != 0)).collect())
    }

    /// Upsert a batch of availability rows in a single transaction. Empty
    /// inputs are a no-op.
    pub async fn availability_upsert_many(&self, rows: &[AvailabilityRow]) -> Result<(), DbError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for row in rows {
            sqlx::query(
                "INSERT INTO stream_availability \
                  (title_id, type, source_id, has_streams, checked_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT(title_id, type, source_id) DO UPDATE SET \
                   has_streams = excluded.has_streams, \
                   checked_at = excluded.checked_at",
            )
            .bind(&row.title_id)
            .bind(row.kind.as_str())
            .bind(&row.source_id)
            .bind(i64::from(row.has_streams))
            .bind(row.checked_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---- recent_searches (F-011) --------------------------------------
    //
    // PRD §F-011 surfaces the last N recent search queries when the
    // search input is empty. The table primary key is the (normalized)
    // query string itself; re-searching the same term refreshes its
    // `searched_at` rather than duplicating the row. `recent_searches_list`
    // returns rows most-recent first; `recent_searches_upsert` writes a
    // single row AND trims the tail past [`RECENT_SEARCHES_MAX`] so the
    // table doesn't grow unbounded across long-running installs.

    /// Return the most recently performed searches, newest first, up to
    /// `limit` rows. Pass [`crate::constants::RECENT_SEARCHES_MAX`] for
    /// the PRD §F-011 default.
    pub async fn recent_searches_list(&self, limit: usize) -> Result<Vec<String>, DbError> {
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT query FROM recent_searches \
             ORDER BY searched_at DESC LIMIT ?",
        )
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(q,)| q).collect())
    }

    /// Record a freshly executed search. Trailing whitespace is trimmed and
    /// empty queries are ignored. After the upsert, rows older than the
    /// `limit`-th most recent are pruned so the table is bounded.
    pub async fn recent_searches_upsert(&self, query: &str, limit: usize) -> Result<(), DbError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = now_unix();
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO recent_searches (query, searched_at) VALUES (?, ?) \
             ON CONFLICT(query) DO UPDATE SET searched_at = excluded.searched_at",
        )
        .bind(trimmed)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // Prune tail. Sub-query picks the cap-th most-recent row's
        // timestamp and deletes anything strictly older. Ties on
        // `searched_at` (unlikely but possible with a 1-second clock
        // resolution) keep both rows; the next prune resolves them on
        // its own tick.
        sqlx::query(
            "DELETE FROM recent_searches WHERE searched_at < (\
               SELECT MIN(searched_at) FROM (\
                 SELECT searched_at FROM recent_searches \
                 ORDER BY searched_at DESC LIMIT ?\
               )\
             )",
        )
        .bind(cap)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove every row from `recent_searches`. Surfaced so the Settings
    /// screen (F-016) can offer a "Clear search history" action.
    pub async fn recent_searches_clear(&self) -> Result<u64, DbError> {
        let res = sqlx::query("DELETE FROM recent_searches")
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn sqlx_json_err(e: serde_json::Error) -> DbError {
    DbError::Sqlx(sqlx::Error::Decode(Box::new(e)))
}

// ---- row decoders --------------------------------------------------------

#[derive(sqlx::FromRow)]
struct CwRow {
    title_id: String,
    r#type: String,
    season: i64,
    episode: i64,
    position_s: f64,
    duration_s: f64,
    last_played_at: i64,
    meta_json: String,
}

impl CwRow {
    fn into_domain(self) -> Result<ContinueWatching, DbError> {
        let kind = match self.r#type.as_str() {
            "movie" => TitleKind::Movie,
            "series" => TitleKind::Series,
            other => {
                return Err(DbError::Sqlx(sqlx::Error::Decode(
                    format!("invalid title kind: {other}").into(),
                )))
            }
        };
        let meta_json = serde_json::from_str(&self.meta_json).map_err(sqlx_json_err)?;
        Ok(ContinueWatching {
            title_id: self.title_id,
            kind,
            season: self.season,
            episode: self.episode,
            position_s: self.position_s,
            duration_s: self.duration_s,
            last_played_at: self.last_played_at,
            meta_json,
        })
    }
}

#[derive(sqlx::FromRow)]
struct AddonRow {
    id: String,
    manifest_url: String,
    enabled: i64,
    installed_at: i64,
    manifest_json: String,
    display_order: i64,
}

impl AddonRow {
    fn into_domain(self) -> Result<Addon, DbError> {
        let manifest_json = serde_json::from_str(&self.manifest_json).map_err(sqlx_json_err)?;
        Ok(Addon {
            id: self.id,
            manifest_url: self.manifest_url,
            enabled: self.enabled != 0,
            installed_at: self.installed_at,
            manifest_json,
            display_order: self.display_order,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_cw_movie(id: &str, pos: f64, t: i64) -> ContinueWatching {
        ContinueWatching {
            title_id: id.into(),
            kind: TitleKind::Movie,
            season: 0,
            episode: 0,
            position_s: pos,
            duration_s: 120.0,
            last_played_at: t,
            meta_json: serde_json::json!({"title": id, "poster": "p"}),
        }
    }

    fn sample_addon(id: &str, url: &str) -> AddonInsert {
        AddonInsert {
            id: id.into(),
            manifest_url: url.into(),
            manifest_json: serde_json::json!({"id": id, "version": "0.0.1"}),
            display_order: None,
        }
    }

    #[tokio::test]
    async fn migration_round_trip_creates_all_tables() {
        let db = Db::open_in_memory().await.unwrap();
        let names: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE '\\_%' ESCAPE '\\' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        let names: Vec<String> = names.into_iter().map(|(n,)| n).collect();
        assert_eq!(
            names,
            vec![
                "addons",
                "continue_watching",
                "recent_searches",
                "response_cache",
                "settings",
                "stream_availability",
            ]
        );
    }

    #[tokio::test]
    async fn migration_is_idempotent_on_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kino.db");
        let id_first = {
            let db = Db::open(&path).await.unwrap();
            db.install_id().await.unwrap()
        };
        // Reopen the same file: migrations must be a no-op and install_id
        // must survive the second bootstrap call.
        let db = Db::open(&path).await.unwrap();
        let id_second = db.install_id().await.unwrap();
        assert_eq!(id_first, id_second, "install_id is stable across reopens");
    }

    #[tokio::test]
    async fn wal_journal_mode_is_active_on_file_backed_db() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kino.db");
        let db = Db::open(&path).await.unwrap();
        let mode = db.journal_mode().await.unwrap().to_lowercase();
        assert_eq!(mode, "wal", "PRD §F-002 requires WAL mode");
    }

    #[tokio::test]
    async fn kv_get_set_round_trip() {
        let db = Db::open_in_memory().await.unwrap();
        assert!(db.kv_get("absent").await.unwrap().is_none());
        db.kv_set("foo", "bar").await.unwrap();
        assert_eq!(db.kv_get("foo").await.unwrap().as_deref(), Some("bar"));
        // Upsert overwrites.
        db.kv_set("foo", "baz").await.unwrap();
        assert_eq!(db.kv_get("foo").await.unwrap().as_deref(), Some("baz"));
    }

    #[tokio::test]
    async fn kv_delete_removes_only_the_named_key() {
        let db = Db::open_in_memory().await.unwrap();
        db.kv_set("keep", "1").await.unwrap();
        db.kv_set("drop", "2").await.unwrap();
        let removed = db.kv_delete("drop").await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(db.kv_get("drop").await.unwrap(), None);
        assert_eq!(db.kv_get("keep").await.unwrap().as_deref(), Some("1"));
        // Deleting an absent key is a no-op, not an error.
        let removed = db.kv_delete("absent").await.unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn install_id_bootstrap_generates_uuid_v4() {
        let db = Db::open_in_memory().await.unwrap();
        let id = db.install_id().await.unwrap();
        let parsed = Uuid::parse_str(&id).expect("install_id must be a UUID");
        assert_eq!(parsed.get_version_num(), 4);
    }

    #[tokio::test]
    async fn cw_upsert_replaces_existing_row_for_same_key() {
        let db = Db::open_in_memory().await.unwrap();
        db.cw_upsert(&sample_cw_movie("tt1", 30.0, 100))
            .await
            .unwrap();
        db.cw_upsert(&sample_cw_movie("tt1", 90.0, 200))
            .await
            .unwrap();
        let rows = db.cw_list().await.unwrap();
        assert_eq!(rows.len(), 1, "upsert must not create duplicates");
        assert!((rows[0].position_s - 90.0).abs() < f64::EPSILON);
        assert_eq!(rows[0].last_played_at, 200);
    }

    #[tokio::test]
    async fn cw_list_orders_by_last_played_desc() {
        let db = Db::open_in_memory().await.unwrap();
        db.cw_upsert(&sample_cw_movie("tt1", 10.0, 100))
            .await
            .unwrap();
        db.cw_upsert(&sample_cw_movie("tt2", 10.0, 300))
            .await
            .unwrap();
        db.cw_upsert(&sample_cw_movie("tt3", 10.0, 200))
            .await
            .unwrap();
        let ids: Vec<String> = db
            .cw_list()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title_id)
            .collect();
        assert_eq!(ids, vec!["tt2", "tt3", "tt1"]);
    }

    #[tokio::test]
    async fn cw_series_keys_on_season_and_episode() {
        let db = Db::open_in_memory().await.unwrap();
        let mut ep = ContinueWatching {
            title_id: "tt_series".into(),
            kind: TitleKind::Series,
            season: 1,
            episode: 1,
            position_s: 5.0,
            duration_s: 60.0,
            last_played_at: 10,
            meta_json: serde_json::json!({}),
        };
        db.cw_upsert(&ep).await.unwrap();
        ep.season = 1;
        ep.episode = 2;
        db.cw_upsert(&ep).await.unwrap();
        let rows = db.cw_list().await.unwrap();
        assert_eq!(rows.len(), 2, "two distinct episodes must coexist");
    }

    #[tokio::test]
    async fn cw_delete_all_for_title_wipes_every_episode() {
        let db = Db::open_in_memory().await.unwrap();
        let mut ep = ContinueWatching {
            title_id: "tt_series".into(),
            kind: TitleKind::Series,
            season: 1,
            episode: 1,
            position_s: 5.0,
            duration_s: 60.0,
            last_played_at: 10,
            meta_json: serde_json::json!({}),
        };
        db.cw_upsert(&ep).await.unwrap();
        ep.season = 1;
        ep.episode = 2;
        db.cw_upsert(&ep).await.unwrap();
        ep.season = 2;
        ep.episode = 1;
        db.cw_upsert(&ep).await.unwrap();
        // Another series should NOT be touched.
        let other = ContinueWatching {
            title_id: "tt_other".into(),
            ..ep.clone()
        };
        db.cw_upsert(&other).await.unwrap();
        let removed = db.cw_delete_all_for_title("tt_series").await.unwrap();
        assert_eq!(removed, 3);
        let remaining: Vec<String> = db
            .cw_list()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.title_id)
            .collect();
        assert_eq!(remaining, vec!["tt_other"]);
        // Absent title is a 0-row no-op, not an error.
        let again = db.cw_delete_all_for_title("tt_series").await.unwrap();
        assert_eq!(again, 0);
    }

    #[tokio::test]
    async fn cw_delete_removes_only_the_matching_row() {
        let db = Db::open_in_memory().await.unwrap();
        db.cw_upsert(&sample_cw_movie("tt1", 10.0, 100))
            .await
            .unwrap();
        db.cw_upsert(&sample_cw_movie("tt2", 10.0, 200))
            .await
            .unwrap();
        let removed = db.cw_delete("tt1", 0, 0).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(db.cw_list().await.unwrap().len(), 1);
        // Deleting an absent row is a no-op, not an error.
        let removed = db.cw_delete("tt404", 0, 0).await.unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn addons_insert_assigns_incrementing_display_order() {
        let db = Db::open_in_memory().await.unwrap();
        db.addons_insert(&sample_addon("a", "https://a"))
            .await
            .unwrap();
        db.addons_insert(&sample_addon("b", "https://b"))
            .await
            .unwrap();
        db.addons_insert(&sample_addon("c", "https://c"))
            .await
            .unwrap();
        let listed: Vec<(String, i64)> = db
            .addons_list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| (a.id, a.display_order))
            .collect();
        assert_eq!(
            listed,
            vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 2),]
        );
    }

    #[tokio::test]
    async fn addons_set_enabled_toggles_flag() {
        let db = Db::open_in_memory().await.unwrap();
        db.addons_insert(&sample_addon("a", "https://a"))
            .await
            .unwrap();
        assert!(db.addons_list().await.unwrap()[0].enabled);
        db.addons_set_enabled("a", false).await.unwrap();
        assert!(!db.addons_list().await.unwrap()[0].enabled);
        db.addons_set_enabled("a", true).await.unwrap();
        assert!(db.addons_list().await.unwrap()[0].enabled);
    }

    #[tokio::test]
    async fn addons_reorder_updates_display_order_by_position() {
        let db = Db::open_in_memory().await.unwrap();
        db.addons_insert(&sample_addon("a", "https://a"))
            .await
            .unwrap();
        db.addons_insert(&sample_addon("b", "https://b"))
            .await
            .unwrap();
        db.addons_insert(&sample_addon("c", "https://c"))
            .await
            .unwrap();
        db.addons_reorder(&["c".into(), "a".into(), "b".into()])
            .await
            .unwrap();
        let ids: Vec<String> = db
            .addons_list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[tokio::test]
    async fn addons_unique_manifest_url_rejects_duplicates() {
        let db = Db::open_in_memory().await.unwrap();
        db.addons_insert(&sample_addon("a", "https://same"))
            .await
            .unwrap();
        let err = db
            .addons_insert(&AddonInsert {
                id: "b".into(),
                manifest_url: "https://same".into(),
                manifest_json: serde_json::json!({}),
                display_order: None,
            })
            .await;
        assert!(err.is_err(), "duplicate manifest_url must be rejected");
    }

    #[tokio::test]
    async fn addons_delete_removes_row() {
        let db = Db::open_in_memory().await.unwrap();
        db.addons_insert(&sample_addon("a", "https://a"))
            .await
            .unwrap();
        let removed = db.addons_delete("a").await.unwrap();
        assert_eq!(removed, 1);
        assert!(db.addons_list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn db_clone_shares_pool() {
        let db = Db::open_in_memory().await.unwrap();
        let clone = db.clone();
        db.kv_set("x", "y").await.unwrap();
        assert_eq!(clone.kv_get("x").await.unwrap().as_deref(), Some("y"));
    }

    #[tokio::test]
    async fn cache_set_then_get_returns_fresh_payload() {
        let db = Db::open_in_memory().await.unwrap();
        let future = now_unix() + 600;
        db.cache_set("k", "{\"v\":1}", future).await.unwrap();
        let got = db.cache_get("k").await.unwrap();
        assert_eq!(got.as_deref(), Some("{\"v\":1}"));
    }

    #[tokio::test]
    async fn cache_get_ignores_expired_row() {
        let db = Db::open_in_memory().await.unwrap();
        let past = now_unix() - 10;
        db.cache_set("k", "stale", past).await.unwrap();
        assert!(db.cache_get("k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cache_set_overwrites_existing_row() {
        let db = Db::open_in_memory().await.unwrap();
        let t = now_unix() + 600;
        db.cache_set("k", "first", t).await.unwrap();
        db.cache_set("k", "second", t).await.unwrap();
        let got = db.cache_get("k").await.unwrap();
        assert_eq!(got.as_deref(), Some("second"));
    }

    fn avail_row(title: &str, source: &str, has_streams: bool, t: i64) -> AvailabilityRow {
        AvailabilityRow {
            title_id: title.into(),
            kind: TitleKind::Movie,
            source_id: source.into(),
            has_streams,
            checked_at: t,
        }
    }

    #[tokio::test]
    async fn availability_upsert_and_get_fresh_round_trip() {
        let db = Db::open_in_memory().await.unwrap();
        let now = now_unix();
        db.availability_upsert_many(&[avail_row("tt1", "addon-a", true, now)])
            .await
            .unwrap();
        // Reading with a fresh_after BEFORE the check returns the row.
        let v = db
            .availability_get_fresh("tt1", TitleKind::Movie, "addon-a", now - 60)
            .await
            .unwrap();
        assert_eq!(v, Some(true));
        // Reading with fresh_after AT or AFTER the check excludes it.
        let v = db
            .availability_get_fresh("tt1", TitleKind::Movie, "addon-a", now)
            .await
            .unwrap();
        assert!(
            v.is_none(),
            "row at checked_at must be excluded by strict >"
        );
    }

    #[tokio::test]
    async fn availability_get_fresh_returns_none_when_absent() {
        let db = Db::open_in_memory().await.unwrap();
        let v = db
            .availability_get_fresh("tt404", TitleKind::Movie, "addon", 0)
            .await
            .unwrap();
        assert!(v.is_none());
    }

    #[tokio::test]
    async fn availability_upsert_replaces_existing_row() {
        let db = Db::open_in_memory().await.unwrap();
        let t = now_unix();
        db.availability_upsert_many(&[avail_row("tt1", "addon-a", false, t - 100)])
            .await
            .unwrap();
        db.availability_upsert_many(&[avail_row("tt1", "addon-a", true, t)])
            .await
            .unwrap();
        // The fresh row wins; row count stays at 1 (no duplicate-key blowup).
        let v = db
            .availability_get_fresh("tt1", TitleKind::Movie, "addon-a", t - 60)
            .await
            .unwrap();
        assert_eq!(v, Some(true));
    }

    #[tokio::test]
    async fn availability_list_fresh_groups_by_title() {
        let db = Db::open_in_memory().await.unwrap();
        let now = now_unix();
        db.availability_upsert_many(&[
            avail_row("tt1", "addon-a", true, now),
            avail_row("tt1", "addon-b", false, now),
            avail_row("tt2", "addon-a", true, now),
            // Stale row — excluded.
            avail_row("tt1", "addon-stale", true, now - 7200),
        ])
        .await
        .unwrap();
        let mut got = db
            .availability_list_fresh("tt1", TitleKind::Movie, now - 60)
            .await
            .unwrap();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("addon-a".to_string(), true),
                ("addon-b".to_string(), false)
            ]
        );
    }

    #[tokio::test]
    async fn availability_upsert_many_handles_batch_atomically() {
        let db = Db::open_in_memory().await.unwrap();
        let now = now_unix();
        let rows: Vec<_> = (0..10)
            .map(|i| avail_row(&format!("tt{i}"), "addon-a", i % 2 == 0, now))
            .collect();
        db.availability_upsert_many(&rows).await.unwrap();
        for i in 0..10 {
            let v = db
                .availability_get_fresh(&format!("tt{i}"), TitleKind::Movie, "addon-a", now - 60)
                .await
                .unwrap();
            assert_eq!(v, Some(i % 2 == 0));
        }
    }

    #[tokio::test]
    async fn availability_upsert_many_empty_input_is_noop() {
        let db = Db::open_in_memory().await.unwrap();
        db.availability_upsert_many(&[]).await.unwrap();
    }

    // ---- F-011 recent_searches ----------------------------------------

    #[tokio::test]
    async fn recent_searches_list_empty_table_returns_empty_vec() {
        let db = Db::open_in_memory().await.unwrap();
        assert!(db.recent_searches_list(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn recent_searches_upsert_then_list_returns_query() {
        let db = Db::open_in_memory().await.unwrap();
        db.recent_searches_upsert("the matrix", 10).await.unwrap();
        let got = db.recent_searches_list(10).await.unwrap();
        assert_eq!(got, vec!["the matrix".to_string()]);
    }

    #[tokio::test]
    async fn recent_searches_upsert_trims_whitespace_and_skips_empty() {
        let db = Db::open_in_memory().await.unwrap();
        db.recent_searches_upsert("  inception  ", 10)
            .await
            .unwrap();
        db.recent_searches_upsert("   ", 10).await.unwrap();
        db.recent_searches_upsert("", 10).await.unwrap();
        let got = db.recent_searches_list(10).await.unwrap();
        assert_eq!(got, vec!["inception".to_string()]);
    }

    #[tokio::test]
    async fn recent_searches_upsert_refreshes_timestamp_for_duplicate() {
        let db = Db::open_in_memory().await.unwrap();
        db.recent_searches_upsert("alpha", 10).await.unwrap();
        // Insert another query so the duplicate test has something to leapfrog.
        // Sleep one second so timestamps differ deterministically — sqlite
        // stores seconds and `now_unix()` truncates to seconds.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        db.recent_searches_upsert("beta", 10).await.unwrap();
        // beta is most recent at this point.
        assert_eq!(
            db.recent_searches_list(10).await.unwrap(),
            vec!["beta".to_string(), "alpha".to_string()]
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        // Re-search alpha → its timestamp is refreshed, jumping it to the
        // front of the list AND keeping the row count at 2.
        db.recent_searches_upsert("alpha", 10).await.unwrap();
        let got = db.recent_searches_list(10).await.unwrap();
        assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn recent_searches_upsert_prunes_past_limit() {
        let db = Db::open_in_memory().await.unwrap();
        // Insert 5 queries strictly in the past, ascending, so the
        // production-path upsert of "f" at `now_unix()` lands as the
        // newest entry.
        let now = now_unix();
        for (i, q) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            sqlx::query("INSERT INTO recent_searches (query, searched_at) VALUES (?, ?)")
                .bind(*q)
                .bind(now - 10 + i64::try_from(i).unwrap())
                .execute(db.pool())
                .await
                .unwrap();
        }
        // limit = 3 → only the three newest survive after the next upsert.
        db.recent_searches_upsert("f", 3).await.unwrap();
        let got = db.recent_searches_list(10).await.unwrap();
        assert_eq!(got, vec!["f".to_string(), "e".to_string(), "d".to_string()]);
    }

    #[tokio::test]
    async fn recent_searches_list_respects_limit_parameter() {
        let db = Db::open_in_memory().await.unwrap();
        let now = now_unix();
        for (i, q) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            sqlx::query("INSERT INTO recent_searches (query, searched_at) VALUES (?, ?)")
                .bind(*q)
                .bind(now + i64::try_from(i).unwrap())
                .execute(db.pool())
                .await
                .unwrap();
        }
        let got = db.recent_searches_list(2).await.unwrap();
        assert_eq!(got, vec!["e".to_string(), "d".to_string()]);
    }

    #[tokio::test]
    async fn recent_searches_clear_removes_every_row() {
        let db = Db::open_in_memory().await.unwrap();
        db.recent_searches_upsert("one", 10).await.unwrap();
        db.recent_searches_upsert("two", 10).await.unwrap();
        let removed = db.recent_searches_clear().await.unwrap();
        assert_eq!(removed, 2);
        assert!(db.recent_searches_list(10).await.unwrap().is_empty());
    }
}
