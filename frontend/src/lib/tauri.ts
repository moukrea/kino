// Typed wrappers around the Tauri command surface defined in
// `src-tauri/src/commands.rs`. Centralizing them here keeps consumer
// routes from sprinkling raw `invoke()` calls across the codebase and
// gives a single place to mock for tests.
//
// PRD §F-008 home-screen feeds: `cwList`, `getTrendingPools`,
// `getWeeklyTrending`. Artwork resolution (`resolveArtwork`) feeds
// the tile thumbnails via F-005. The full Tauri surface is larger
// (addons CRUD, credential tests, availability) but those callers
// land in their own feature sessions.

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

export type TitleKind = "movie" | "series";

export type TitleSummary = {
  id: string;
  kind: TitleKind;
  title: string;
  year: number | null;
  poster: string | null;
  rating: number | null;
};

export type TrendingPools = {
  top_trending: TitleSummary[];
  hidden_gems: TitleSummary[];
};

export type ContinueWatching = {
  title_id: string;
  kind: TitleKind;
  season: number;
  episode: number;
  position_s: number;
  duration_s: number;
  last_played_at: number;
  meta_json: unknown;
};

export type ArtworkProvenance = {
  poster: string;
  backdrop: string;
  logo: string;
  clearart: string;
  summary: string;
};

export type Artwork = {
  poster: string;
  backdrop: string;
  logo: string;
  clearart: string;
  summary: string;
  sources: ArtworkProvenance;
};

/**
 * One PRD §F-008 row 5 catalog row served by an installed addon. Each
 * row maps 1:1 to a [`Row`](../components/Row) on the home screen,
 * rendered under the four locked rows in addon `display_order` then
 * catalog order within each addon.
 */
export type HomeCatalog = {
  addon_id: string;
  addon_name: string;
  catalog_id: string;
  catalog_kind: TitleKind | string;
  catalog_name: string;
  items: TitleSummary[];
};

/**
 * Detect whether the Tauri IPC bridge is reachable. Used by routes to
 * decide between live data and an in-browser placeholder when the
 * frontend bundle is opened in a plain `vite dev` without the Tauri
 * host (e.g. design iteration, vitest jsdom).
 */
export function hasTauri(): boolean {
  if (typeof window === "undefined") return false;
  // The Tauri 2 runtime installs `__TAURI_INTERNALS__` on `window`.
  return Boolean(
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
  );
}

export async function cwList(): Promise<ContinueWatching[]> {
  return invoke<ContinueWatching[]>("cw_list");
}

export async function getTrendingPools(
  kind: TitleKind,
  locale: string,
): Promise<TrendingPools> {
  return invoke<TrendingPools>("get_trending_pools", { kind, locale });
}

export async function getWeeklyTrending(
  kind: TitleKind,
  locale: string,
): Promise<TitleSummary[]> {
  return invoke<TitleSummary[]>("get_weekly_trending", { kind, locale });
}

export async function resolveArtwork(
  titleId: string,
  kind: TitleKind,
  langPref: string[],
): Promise<Artwork> {
  return invoke<Artwork>("resolve_artwork", {
    titleId,
    kind,
    langPref,
  });
}

/**
 * `list_home_catalogs(kind, locale)` — PRD §F-008 row 5 + §F-009 filter.
 *
 * Returns the dynamic addon-catalog rows that appear under the four
 * locked rows on the home screen. Pass `kind = null` for the unfiltered
 * Home (every catalog of every enabled addon) or `"movie"` / `"series"`
 * for the sub-home filtered variants. Catalogs that fail to fetch or
 * return zero items are filtered out by the backend.
 */
export async function listHomeCatalogs(
  kind: TitleKind | null,
  locale: string,
): Promise<HomeCatalog[]> {
  return invoke<HomeCatalog[]>("list_home_catalogs", { kind, locale });
}

// ---- F-010: Title detail view -----------------------------------------

export type CastMember = {
  name: string;
  character: string | null;
  photo: string | null;
};

export type Episode = {
  video_id: string;
  season: number;
  episode: number;
  title: string;
  air_date: string | null;
  overview: string | null;
  thumbnail: string | null;
  /** Watch progress in `[0.0, 1.0]`; zero when no CW entry exists. */
  progress: number;
};

export type TitleDetail = {
  id: string;
  kind: TitleKind;
  title: string;
  year: number | null;
  runtime_minutes: number | null;
  age_rating: string | null;
  genres: string[];
  summary: string | null;
  imdb_rating: number | null;
  tmdb_rating: number | null;
  trakt_rating: number | null;
  backdrop: string | null;
  logo: string | null;
  poster: string | null;
  cast: CastMember[];
  episodes: Episode[];
  /** When present, the Resume button is shown (PRD §F-010 acceptance). */
  resume_position_s: number | null;
  resume_duration_s: number | null;
  resume_season: number | null;
  resume_episode: number | null;
  resume_video_id: string | null;
  stremio_id: string | null;
};

export type StreamQuality = "4K" | "1080p" | "720p" | "SD";
export type StreamHdr = "DV" | "HDR10+" | "HDR10";
export type StreamAudio =
  | "ATMOS"
  | "TRUEHD"
  | "DTSHD"
  | "DTSX"
  | "EAC3"
  | "AC3"
  | "DTS";
export type StreamCodec = "AV1" | "H265" | "H264";

export type StreamRow = {
  addon_id: string;
  addon_name: string;
  name: string;
  detail: string | null;
  quality: StreamQuality | null;
  hdr: StreamHdr | null;
  audio: StreamAudio | null;
  codec: StreamCodec | null;
  seeders: number | null;
  size_bytes: number | null;
  url: string | null;
  info_hash: string | null;
  file_idx: number | null;
  sources: string[];
};

export async function getTitleDetail(
  titleId: string,
  kind: TitleKind,
  langPref: string[],
): Promise<TitleDetail> {
  return invoke<TitleDetail>("get_title_detail", {
    titleId,
    kind,
    langPref,
  });
}

export async function getStreams(
  titleId: string,
  kind: TitleKind,
  season: number | null,
  episode: number | null,
): Promise<StreamRow[]> {
  return invoke<StreamRow[]>("get_streams", {
    titleId,
    kind,
    season,
    episode,
  });
}

export async function cwUpsert(entry: ContinueWatching): Promise<void> {
  return invoke<void>("cw_upsert", { entry });
}

export async function cwDelete(
  titleId: string,
  season: number,
  episode: number,
): Promise<number> {
  return invoke<number>("cw_delete", { titleId, season, episode });
}

/**
 * PRD §F-012 canonical position writer. Applies completion + series
 * next-episode rules backend-side. `episodes` is the canonical
 * `(season, episode)` tuple list (empty for movies). Returns the row
 * that ends up on disk, or `null` when the rule wiped the series.
 */
export async function cwRecordPosition(
  entry: ContinueWatching,
  episodes: ReadonlyArray<readonly [number, number]>,
): Promise<ContinueWatching | null> {
  return invoke<ContinueWatching | null>("cw_record_position", {
    entry,
    episodes,
  });
}

/**
 * PRD §F-012 manual-remove action. Wipes every CW row for `titleId` —
 * triggered by Y / Menu / right-click / long-press on a Home CW tile.
 */
export async function cwRemoveTitle(titleId: string): Promise<number> {
  return invoke<number>("cw_remove_title", { titleId });
}

// ---- F-011: Search ---------------------------------------------------

/**
 * IMDb-id shortcut hit. Present on the search response when the typed
 * query matches `^tt\d+$` and TMDB `/find?external_source=imdb_id`
 * resolves it to a movie or series. The UI MUST navigate to the
 * `/title/:id` route immediately rather than render the result list
 * (PRD §F-011 acceptance: "Pasting `tt1234567` opens the corresponding
 * title detail directly").
 */
export type SearchDirectMatch = {
  /** Provider-prefixed kino id (`imdb:ttN`). Use as-is in the route. */
  id: string;
  /** Detected kind so the detail route knows which IPC to issue. */
  kind: TitleKind;
};

export type SearchResponse = {
  direct: SearchDirectMatch | null;
  results: TitleSummary[];
  /** True when at least one extra candidate exists past this page. */
  has_more: boolean;
};

/**
 * `search(query, page, locale)` — PRD §F-011. Returns aggregated,
 * deduped, availability-filtered results. Empty / whitespace-only
 * queries resolve to an empty response (the UI surfaces recent
 * searches via `recentSearchesList` in that case).
 */
export async function search(
  query: string,
  page: number,
  locale: string,
): Promise<SearchResponse> {
  return invoke<SearchResponse>("search", { query, page, locale });
}

/**
 * `recent_searches_list()` — newest first, up to RECENT_SEARCHES_MAX
 * (10) entries.
 */
export async function recentSearchesList(): Promise<string[]> {
  return invoke<string[]>("recent_searches_list");
}

/**
 * `recent_searches_upsert(query)` — refresh the entry's timestamp.
 * Idempotent. Skipped server-side for empty queries.
 */
export async function recentSearchesUpsert(query: string): Promise<void> {
  return invoke<void>("recent_searches_upsert", { query });
}

/**
 * `recent_searches_clear()` — remove every recent-searches row.
 * Returns the number of rows removed.
 */
export async function recentSearchesClear(): Promise<number> {
  return invoke<number>("recent_searches_clear");
}

// ---- F-016: Settings -------------------------------------------------

export type ApiKeysView = {
  tmdb: string;
  trakt: string;
  tvdb: string;
  fanart: string;
};

export type LanguageView = {
  metadata_primary: string;
  metadata_fallback: string[];
  ui: string;
};

export type CacheView = {
  path: string;
  size_gib: number;
  min_gib: number;
  max_gib: number;
};

export type BufferView = {
  safety_margin_s: number;
  prebuffer_target_s: number;
  piece_high_s: number;
  piece_med_s: number;
  recompute_interval_s: number;
};

export type PlayerView = {
  passthrough_truehd: boolean;
  passthrough_dtshd: boolean;
  passthrough_dtsx: boolean;
  passthrough_atmos: boolean;
  passthrough_ac3: boolean;
  passthrough_dts: boolean;
  passthrough_eac3: boolean;
  force_hw_decode: boolean;
  tunneling: boolean;
};

export type DisplayView = {
  tile_size: "small" | "medium" | "large" | string;
  focus_animation: boolean;
  nsfw: boolean;
  input_override: "auto" | "touch" | "dpad" | "kbm" | string;
  high_contrast: boolean;
  /**
   * PRD §5 Logging "advanced logging" toggle. When true, the host
   * switches the runtime `tracing` `EnvFilter` to `debug`.
   */
  advanced_logging: boolean;
};

export type SettingsView = {
  api_keys: ApiKeysView;
  language: LanguageView;
  cache: CacheView;
  buffer: BufferView;
  player: PlayerView;
  display: DisplayView;
};

export type AppInfo = {
  name: string;
  version: string;
  commit: string;
  repository: string;
  license: string;
  platform: string;
};

/**
 * KV keys for every PRD §F-016 setting. Kept in lockstep with
 * `src-tauri/src/settings.rs::KNOWN_SETTINGS_KEYS`. The Settings UI uses
 * these constants when calling `settingsSet`.
 */
export const SETTING_KEYS = {
  apiTmdb: "tmdb_api_key",
  apiTrakt: "trakt_api_key",
  apiTvdb: "tvdb_api_key",
  apiFanart: "fanart_api_key",
  metaPrimaryLang: "lang.metadata_primary",
  metaFallbackLangs: "lang.metadata_fallback",
  uiLang: "lang.ui",
  cachePath: "cache.path",
  cacheSizeGib: "cache.size_gib",
  bufferSafetyMarginS: "buffer.safety_margin_s",
  bufferPrebufferTargetS: "buffer.prebuffer_target_s",
  bufferPieceHighS: "buffer.piece_high_s",
  bufferPieceMedS: "buffer.piece_med_s",
  bufferRecomputeIntervalS: "buffer.recompute_interval_s",
  playerPassthroughTruehd: "player.passthrough.truehd",
  playerPassthroughDtshd: "player.passthrough.dtshd",
  playerPassthroughDtsx: "player.passthrough.dtsx",
  playerPassthroughAtmos: "player.passthrough.atmos",
  playerPassthroughAc3: "player.passthrough.ac3",
  playerPassthroughDts: "player.passthrough.dts",
  playerPassthroughEac3: "player.passthrough.eac3",
  playerForceHwDecode: "player.force_hw_decode",
  playerTunneling: "player.tunneling",
  displayTileSize: "display.tile_size",
  displayFocusAnimation: "display.focus_animation",
  displayNsfw: "display.nsfw",
  displayInputOverride: "display.input_override",
  displayHighContrast: "display.high_contrast",
  displayAdvancedLogging: "display.advanced_logging",
} as const;

export async function settingsGetAll(): Promise<SettingsView> {
  return invoke<SettingsView>("settings_get_all");
}

export async function settingsSet(key: string, value: string): Promise<string> {
  return invoke<string>("settings_set", { key, value });
}

export async function settingsResetDefaults(): Promise<void> {
  return invoke<void>("settings_reset_defaults");
}

export async function cacheUsageBytes(): Promise<number> {
  return invoke<number>("cache_usage_bytes");
}

export async function cacheClear(): Promise<void> {
  return invoke<void>("cache_clear");
}

export async function exportLogs(destZip: string): Promise<number> {
  return invoke<number>("export_logs", { destZip });
}

/**
 * PRD §F-016 §4 Cache → Path: native directory picker. Wraps
 * `@tauri-apps/plugin-dialog`'s `open({ directory: true })` so callers
 * only see a `Promise<string | null>` (null when the user cancels or
 * when the Tauri runtime isn't reachable in plain-vite/jsdom).
 */
export async function pickDirectory(
  initialPath?: string,
): Promise<string | null> {
  if (!hasTauri()) return null;
  const result = await openDialog({
    directory: true,
    multiple: false,
    defaultPath: initialPath && initialPath.length > 0 ? initialPath : undefined,
  });
  return typeof result === "string" ? result : null;
}

export async function getAppInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("get_app_info");
}

// ---- F-003 credential tests + F-007 addon controls (used by F-016) -----

export async function testTmdb(): Promise<void> {
  return invoke<void>("test_tmdb");
}
export async function testTrakt(): Promise<void> {
  return invoke<void>("test_trakt");
}
export async function testTvdb(): Promise<void> {
  return invoke<void>("test_tvdb");
}
export async function testFanart(): Promise<void> {
  return invoke<void>("test_fanart");
}

export type AddonRow = {
  id: string;
  manifest_url: string;
  enabled: boolean;
  installed_at: number;
  manifest_json: unknown;
  display_order: number;
};

export type RecommendedAddon = {
  name: string;
  manifest_url: string;
  description: string;
};

export async function addonsList(): Promise<AddonRow[]> {
  return invoke<AddonRow[]>("addons_list");
}

export async function addonsSetEnabled(
  id: string,
  enabled: boolean,
): Promise<number> {
  return invoke<number>("addons_set_enabled", { id, enabled });
}

export async function getRecommendedAddons(): Promise<RecommendedAddon[]> {
  return invoke<RecommendedAddon[]>("get_recommended_addons");
}

export async function installAddon(url: string): Promise<AddonRow> {
  return invoke<AddonRow>("install_addon", { url });
}

export async function uninstallAddon(id: string): Promise<number> {
  return invoke<number>("uninstall_addon", { id });
}

export async function setAddonOrder(id: string, order: number): Promise<void> {
  return invoke<void>("set_addon_order", { id, order });
}

// ---- F-013: embedded torrent engine + local HTTP server -----------------

/**
 * One file inside an added torrent, surfaced by `startPlayback` for the
 * UI's "wrong file picked?" affordance. Empty for direct-URL playback.
 */
export type PlaybackFile = {
  index: number;
  relativePath: string;
  size: number;
  isVideo: boolean;
};

/**
 * Response shape of `start_playback`. The `url` is what the platform
 * player consumes (a local HTTP URL for torrent-backed playback, the
 * passthrough URL for direct streams). `token` is the handle that
 * `stop_playback` / `playback_status` keys off.
 */
export type PlaybackHandle = {
  url: string;
  /** Empty string for direct URLs. */
  token: string;
  /** `true` iff the embedded engine is serving this stream. */
  viaTorrent: boolean;
  fileName: string;
  fileSize: number | null;
  mime: string | null;
  infoHash: string | null;
  files: PlaybackFile[];
  torrentId: number | null;
};

/**
 * Discriminated input for `startPlayback`. The frontend selects:
 *
 * - `magnet`: `magnet:?xt=urn:btih:…` URI (or a `.torrent` http link).
 * - `torrentBytes`: raw `.torrent` file bytes, base64-encoded.
 * - `directUrl`: a pre-resolved HTTP(S) URL (used for the http-stream
 *   addon variants — no torrent engine involvement).
 */
export type PlaybackSource =
  | { kind: "magnet"; url: string; fileIndex?: number | null }
  | {
      kind: "torrentBytes";
      bytesBase64: string;
      fileIndex?: number | null;
    }
  | {
      kind: "directUrl";
      url: string;
      mime?: string | null;
      fileName?: string | null;
    };

/**
 * `start_playback` (PRD §F-013). Adds the torrent (waiting up to the
 * engine's `init_timeout` for metadata), picks the largest video file
 * unless the caller supplies a `fileIndex`, registers a token with the
 * local HTTP server, and returns the streaming URL.
 */
export async function startPlayback(
  source: PlaybackSource,
): Promise<PlaybackHandle> {
  return invoke<PlaybackHandle>("start_playback", { source });
}

/**
 * `stop_playback(token, deleteFiles?)` — tears down the registered
 * session and removes the torrent from the engine. `deleteFiles`
 * defaults to `false` so the next Play on the same title reuses the
 * already-downloaded cache.
 */
export async function stopPlayback(
  token: string,
  deleteFiles?: boolean,
): Promise<boolean> {
  return invoke<boolean>("stop_playback", { token, deleteFiles });
}

/**
 * `playback_status(token)` snapshot. The F-015 player consumes it to
 * surface filename + size before bytes start flowing.
 */
export type PlaybackStatus = {
  token: string;
  fileName: string;
  fileSize: number;
  active: boolean;
};

export async function playbackStatus(
  token: string,
): Promise<PlaybackStatus | null> {
  return invoke<PlaybackStatus | null>("playback_status", { token });
}

// ---- F-014: adaptive buffer ---------------------------------------------

/**
 * Wire-shaped payload of the `buffer:status` Tauri event (PRD §F-014).
 * Mirrors `BufferStatusEvent` in `src-tauri/src/commands.rs`. The player
 * subscribes via [`onBufferStatus`] and renders the "Buffering for smooth
 * playback" overlay whenever `state !== "safe"`.
 */
export type BufferStatusEvent = {
  token: string;
  state: "safe" | "needsPrebuffer" | "rebuffer";
  /** Present when `state === "needsPrebuffer"`. */
  requiredPrebufferS: number | null;
  dlRateBytesPerS: number;
  piecesAheadSeconds: number;
  bytesDownloaded: number;
  fileSizeBytes: number;
  positionS: number;
  durationS: number;
  etaSeconds: number | null;
};

/**
 * `buffer_start_monitor(token, durationS)` (PRD §F-014). Spins up the
 * adaptive-buffer state machine + sampler for an in-flight playback
 * session and starts emitting `buffer:status` events every 5 s plus on
 * every `bufferReportPosition` call. Idempotent — calling twice on the
 * same token replaces the prior monitor.
 */
export async function bufferStartMonitor(
  token: string,
  durationS: number,
): Promise<void> {
  await invoke<void>("buffer_start_monitor", { token, durationS });
}

/**
 * `buffer_stop_monitor(token)` (PRD §F-014). Tears down the monitor +
 * event bridge for the given token. Returns `true` if a monitor was
 * active. Called on player exit. `stop_playback` also auto-stops the
 * monitor, so explicit teardown is only needed when the player wants to
 * stop monitoring without stopping the underlying stream.
 */
export async function bufferStopMonitor(token: string): Promise<boolean> {
  return invoke<boolean>("buffer_stop_monitor", { token });
}

/**
 * `buffer_report_position(token, positionS)` (PRD §F-014). Pushes a fresh
 * playhead value into the monitor. PRD §8 caps cadence at
 * `PLAYER_POSITION_INTERVAL_S = 5s`; the player calls this every 5 s and
 * on every seek so the state machine recomputes immediately.
 */
export async function bufferReportPosition(
  token: string,
  positionS: number,
): Promise<boolean> {
  return invoke<boolean>("buffer_report_position", { token, positionS });
}

/**
 * `buffer_status(token)` — one-shot snapshot of the monitor's current
 * state. Returns `null` if no monitor is registered for `token`. The
 * player uses this for its first paint so the overlay shows correct data
 * before the first recompute event arrives.
 */
export async function bufferStatus(
  token: string,
): Promise<BufferStatusEvent | null> {
  return invoke<BufferStatusEvent | null>("buffer_status", { token });
}

/**
 * Subscribe to the `buffer:status` Tauri event. Returns a `Promise` of
 * an `unlisten` function; callers should call it from `onCleanup` to
 * tear down the listener.
 *
 * Resolves to a no-op `unlisten` when the Tauri bridge isn't present
 * (vitest jsdom / plain `vite dev`), so consumer code can `await` it
 * unconditionally.
 */
export async function onBufferStatus(
  handler: (status: BufferStatusEvent) => void,
): Promise<() => void> {
  if (!hasTauri()) {
    return () => {};
  }
  // Lazy import so the @tauri-apps/api/event chunk is only pulled into
  // bundles that actually wire the player UI.
  const { listen } = await import("@tauri-apps/api/event");
  return listen<BufferStatusEvent>("buffer:status", (e) => handler(e.payload));
}

// ---- F-015: native player driver ----------------------------------------

/**
 * Possible player states. Camel-cased on the wire to match the Rust
 * `PlayerState` enum and the Tauri event payloads from
 * `src-tauri/src/commands.rs` / `kino_player::PlayerState`.
 */
export type PlayerState =
  | "idle"
  | "loading"
  | "playing"
  | "paused"
  | "buffering"
  | "ended"
  | "error";

/**
 * Snapshot returned by [`playerStatus`]. Mirrors
 * `kino_player::PlayerSnapshot`.
 */
export type PlayerSnapshot = {
  token: string;
  state: PlayerState;
  positionS: number;
  durationS: number;
  paused: boolean;
};

/**
 * Audio / subtitle track descriptor returned by the player. Camel-cased
 * mirror of `kino_player::AudioTrack` / `SubtitleTrack`.
 */
export type AudioTrack = {
  id: number;
  title: string | null;
  language: string | null;
  codec: string | null;
  channels: number | null;
  isDefault: boolean;
  isSelected: boolean;
};

export type SubtitleTrack = {
  id: number;
  title: string | null;
  language: string | null;
  codec: string | null;
  isDefault: boolean;
  isForced: boolean;
  isSelected: boolean;
};

export type PlayerTrackList = {
  audio: AudioTrack[];
  subtitles: SubtitleTrack[];
};

export type PlayerStatusResponse = {
  snapshot: PlayerSnapshot;
  tracks: PlayerTrackList;
};

/**
 * Continue-Watching context attached to a `playerOpen` call. The
 * backend fans every position tick + the terminal Exit event into
 * `cw_record_position` so the user's Continue Watching row is always
 * up to date without the frontend issuing its own writes.
 */
export type PlayerCwContext = {
  titleId: string;
  kind: "movie" | "series";
  season: number;
  episode: number;
  metaJson: unknown;
  /** Sorted list of `[season, episode]` tuples for next-episode advance. */
  episodes: [number, number][];
};

/**
 * Wire request for `playerOpen`. Mirrors `PlayerOpenRequest` in
 * `src-tauri/src/commands.rs`.
 */
export type PlayerOpenRequest = {
  token: string;
  url: string;
  resumePositionS: number;
  fileName?: string | null;
  durationHintS?: number | null;
  cwContext?: PlayerCwContext | null;
};

/**
 * `player_open` (PRD §F-015). Boots the platform driver (mpv on Linux;
 * the Tauri-plugin-wrapped `PlayerActivity` on Android) and starts
 * playback. Idempotent — calling twice closes the prior session first.
 */
export async function playerOpen(request: PlayerOpenRequest): Promise<void> {
  await invoke<void>("player_open", { request });
}

/**
 * `player_close` — close the active session. Returns `false` when
 * nothing was running.
 */
export async function playerClose(): Promise<boolean> {
  return invoke<boolean>("player_close");
}

/** `player_pause(paused)` — toggle the pause state. */
export async function playerPause(paused: boolean): Promise<void> {
  await invoke<void>("player_pause", { paused });
}

/** `player_seek(positionS)` — seek to an absolute time in seconds. */
export async function playerSeek(positionS: number): Promise<void> {
  await invoke<void>("player_seek", { positionS });
}

/** `player_set_audio_track(trackId)` — select an audio track (null = disable). */
export async function playerSetAudioTrack(
  trackId: number | null,
): Promise<void> {
  await invoke<void>("player_set_audio_track", { trackId });
}

/** `player_set_subtitle_track(trackId)` — select a subtitle track (null = disable). */
export async function playerSetSubtitleTrack(
  trackId: number | null,
): Promise<void> {
  await invoke<void>("player_set_subtitle_track", { trackId });
}

/** `player_status()` — snapshot of the active session, or `null`. */
export async function playerStatus(): Promise<PlayerStatusResponse | null> {
  return invoke<PlayerStatusResponse | null>("player_status");
}

/**
 * Discriminated player-event payload (PRD §F-015). Tauri events are
 * emitted on the channels `player:position`, `player:state`,
 * `player:tracks`, `player:exit`, `player:error`. Subscribers can
 * use [`onPlayerEvent`] to subscribe to all of them at once, or the
 * channel-specific helpers below.
 */
export type PlayerEvent =
  | {
      kind: "position";
      positionS: number;
      durationS: number;
      paused: boolean;
    }
  | { kind: "state"; state: PlayerState }
  | { kind: "tracks"; tracks: PlayerTrackList }
  | {
      kind: "exit";
      positionS: number;
      durationS: number;
      reachedEof: boolean;
    }
  | { kind: "error"; message: string };

/**
 * Subscribe to one of the `player:*` Tauri events. Returns a Promise
 * of an `unlisten` function the caller should invoke from `onCleanup`.
 *
 * Resolves to a no-op `unlisten` when the Tauri bridge isn't present
 * (vitest jsdom / plain `vite dev`), so consumer code can `await` it
 * unconditionally.
 */
async function subscribePlayerEvent<T extends PlayerEvent>(
  channel: string,
  handler: (event: T) => void,
): Promise<() => void> {
  if (!hasTauri()) {
    return () => {};
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<T>(channel, (e) => handler(e.payload));
}

export async function onPlayerPosition(
  handler: (event: Extract<PlayerEvent, { kind: "position" }>) => void,
): Promise<() => void> {
  return subscribePlayerEvent("player:position", handler);
}

export async function onPlayerState(
  handler: (event: Extract<PlayerEvent, { kind: "state" }>) => void,
): Promise<() => void> {
  return subscribePlayerEvent("player:state", handler);
}

export async function onPlayerTracks(
  handler: (event: Extract<PlayerEvent, { kind: "tracks" }>) => void,
): Promise<() => void> {
  return subscribePlayerEvent("player:tracks", handler);
}

export async function onPlayerExit(
  handler: (event: Extract<PlayerEvent, { kind: "exit" }>) => void,
): Promise<() => void> {
  return subscribePlayerEvent("player:exit", handler);
}

export async function onPlayerError(
  handler: (event: Extract<PlayerEvent, { kind: "error" }>) => void,
): Promise<() => void> {
  return subscribePlayerEvent("player:error", handler);
}
