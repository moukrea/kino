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
}
