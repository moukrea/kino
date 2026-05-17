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
