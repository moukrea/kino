-- 0001_init.sql — initial schema (PRD §F-002).
-- This migration is locked content; changes require a human PRD revision.
-- Applied by `sqlx::migrate!()` from the F-002 implementation session.

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE addons (
  id TEXT PRIMARY KEY,
  manifest_url TEXT NOT NULL UNIQUE,
  enabled INTEGER NOT NULL DEFAULT 1,
  installed_at INTEGER NOT NULL,
  manifest_json TEXT NOT NULL,
  display_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE continue_watching (
  title_id TEXT NOT NULL,
  type TEXT NOT NULL CHECK(type IN ('movie','series')),
  season INTEGER NOT NULL DEFAULT 0,
  episode INTEGER NOT NULL DEFAULT 0,
  position_s REAL NOT NULL,
  duration_s REAL NOT NULL,
  last_played_at INTEGER NOT NULL,
  meta_json TEXT NOT NULL,
  PRIMARY KEY (title_id, season, episode)
);
CREATE INDEX idx_continue_watching_last_played ON continue_watching(last_played_at DESC);

CREATE TABLE response_cache (
  key TEXT PRIMARY KEY,
  payload_json TEXT NOT NULL,
  etag TEXT,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_response_cache_expires ON response_cache(expires_at);

CREATE TABLE stream_availability (
  title_id TEXT NOT NULL,
  type TEXT NOT NULL,
  source_id TEXT NOT NULL,
  has_streams INTEGER NOT NULL,
  checked_at INTEGER NOT NULL,
  PRIMARY KEY (title_id, type, source_id)
);
CREATE INDEX idx_stream_availability_checked ON stream_availability(checked_at);

CREATE TABLE recent_searches (
  query TEXT PRIMARY KEY,
  searched_at INTEGER NOT NULL
);
