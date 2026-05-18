// PRD §F-009 Movies / Series sub-home behavior plus the unfiltered
// Home's mixed-feed rendering. Mounts `HomeView` with each of the three
// `kind` values against a mocked Tauri command surface and asserts:
//
//   - Movies (`kind="movie"`) renders only movie tiles
//   - Series (`kind="series"`) renders only series tiles
//   - Home (`kind=null`) renders both kinds, interleaved
//   - CW row hides per-kind when no entries match the active kind
//   - `interleaveByKind` alternates input lists at index granularity
//
// Mocking strategy: `vi.mock("../lib/tauri", ...)` overrides
// `hasTauri()` to true and replaces the four data functions with
// per-test fakes. Without the override, `hasTauri()` returns false in
// jsdom and HomeView's resources fall through to empty data (which
// `Home.test.tsx` already covers for the row-order acceptance).

import { render } from "solid-js/web";
import { MemoryRouter } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HomeView, interleaveByKind } from "./Home";
import { _resetForTests as _resetFocus } from "../input/focus";
import { _resetForTests as _resetProfile } from "../input/profile";
import type {
  ContinueWatching,
  HomeCatalog,
  TitleKind,
  TitleSummary,
  TrendingPools,
} from "../lib/tauri";

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return {
    ...actual,
    hasTauri: () => true,
    cwList: vi.fn(),
    cwRemoveTitle: vi.fn(),
    getTrendingPools: vi.fn(),
    getWeeklyTrending: vi.fn(),
    listHomeCatalogs: vi.fn(),
    checkAvailability: vi.fn(),
  };
});

const tauri = await import("../lib/tauri");
const mockedCwList = vi.mocked(tauri.cwList);
const mockedCwRemoveTitle = vi.mocked(tauri.cwRemoveTitle);
const mockedGetTrendingPools = vi.mocked(tauri.getTrendingPools);
const mockedGetWeeklyTrending = vi.mocked(tauri.getWeeklyTrending);
const mockedListHomeCatalogs = vi.mocked(tauri.listHomeCatalogs);
const mockedCheckAvailability = vi.mocked(tauri.checkAvailability);

const displaySettings = await import("../lib/displaySettings");

function summary(id: string, kind: TitleKind, title: string): TitleSummary {
  return { id, kind, title, year: 2024, poster: null, rating: null };
}

function pools(
  top: TitleSummary[],
  gems: TitleSummary[] = [],
): TrendingPools {
  return { top_trending: top, hidden_gems: gems };
}

function cw(title_id: string, kind: TitleKind): ContinueWatching {
  return {
    title_id,
    kind,
    season: 0,
    episode: 0,
    position_s: 0,
    duration_s: 0,
    last_played_at: 0,
    meta_json: { title: title_id, year: 2024, poster: null },
  };
}

function cwEpisode(
  title_id: string,
  season: number,
  episode: number,
  position: number,
  duration: number,
): ContinueWatching {
  return {
    title_id,
    kind: "series",
    season,
    episode,
    position_s: position,
    duration_s: duration,
    last_played_at: 0,
    meta_json: { title: title_id, year: 2024, poster: null },
  };
}

function catalog(
  addonId: string,
  addonName: string,
  catalogId: string,
  catalogKind: TitleKind,
  catalogName: string,
  items: TitleSummary[],
): HomeCatalog {
  return {
    addon_id: addonId,
    addon_name: addonName,
    catalog_id: catalogId,
    catalog_kind: catalogKind,
    catalog_name: catalogName,
    items,
  };
}

async function flushAsync() {
  // Resolve the createResource fetcher microtasks and the queueMicrotask
  // chain that follows. Two passes is enough because the fetcher chains
  // are Promise-based and SolidJS commits synchronously after each.
  await Promise.resolve();
  await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
}

function mount(host: HTMLElement, kind: TitleKind | null) {
  return render(
    () => (
      <MemoryRouter root={() => <HomeView kind={kind} />}>
        <></>
      </MemoryRouter>
    ),
    host,
  );
}

describe("HomeView (F-009)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    displaySettings._resetForTests();
    mockedCwList.mockReset();
    mockedCwRemoveTitle.mockReset();
    mockedGetTrendingPools.mockReset();
    mockedGetWeeklyTrending.mockReset();
    mockedListHomeCatalogs.mockReset();
    mockedCheckAvailability.mockReset();
    mockedCwList.mockResolvedValue([]);
    mockedCwRemoveTitle.mockResolvedValue(0);
    mockedGetTrendingPools.mockResolvedValue(pools([]));
    mockedGetWeeklyTrending.mockResolvedValue([]);
    mockedListHomeCatalogs.mockResolvedValue([]);
    // Default: every check_availability batch reports every requested
    // (kind, id) as available, so the F-009 sub-home tests can keep
    // ignoring the filter (it was a no-op before and stays a no-op).
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: true,
        source_count: 1,
      })),
    );
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("Movies sub-home renders only movie tiles", async () => {
    mockedGetTrendingPools.mockImplementation(async (kind) =>
      kind === "movie"
        ? pools([summary("m1", "movie", "Matrix")], [
            summary("m2", "movie", "Inception"),
          ])
        : pools([summary("s1", "series", "Breaking Bad")]),
    );
    mockedGetWeeklyTrending.mockImplementation(async (kind) =>
      kind === "movie" ? [summary("m3", "movie", "Heat")] : [],
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>("button[data-kind]"),
    );
    expect(tiles.length).toBeGreaterThan(0);
    for (const tile of tiles) {
      expect(tile.dataset.kind).toBe("movie");
    }
  });

  it("Series sub-home renders only series tiles", async () => {
    mockedGetTrendingPools.mockImplementation(async (kind) =>
      kind === "series"
        ? pools([summary("s1", "series", "Breaking Bad")], [
            summary("s2", "series", "The Wire"),
          ])
        : pools([summary("m1", "movie", "Matrix")]),
    );
    mockedGetWeeklyTrending.mockImplementation(async (kind) =>
      kind === "series" ? [summary("s3", "series", "Severance")] : [],
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "series");
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>("button[data-kind]"),
    );
    expect(tiles.length).toBeGreaterThan(0);
    for (const tile of tiles) {
      expect(tile.dataset.kind).toBe("series");
    }
  });

  it("unfiltered Home renders both movie and series tiles", async () => {
    mockedGetTrendingPools.mockImplementation(async (kind) =>
      kind === "movie"
        ? pools([summary("m1", "movie", "Matrix")])
        : pools([summary("s1", "series", "Breaking Bad")]),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>("button[data-kind]"),
    );
    const kinds = new Set(tiles.map((t) => t.dataset.kind));
    expect(kinds).toContain("movie");
    expect(kinds).toContain("series");
  });

  it("Movies sub-home calls trending and weekly with kind='movie' only", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    const trendingKinds = mockedGetTrendingPools.mock.calls.map((c) => c[0]);
    const weeklyKinds = mockedGetWeeklyTrending.mock.calls.map((c) => c[0]);
    expect(trendingKinds).toEqual(["movie"]);
    expect(weeklyKinds).toEqual(["movie"]);
  });

  it("unfiltered Home fires both kinds in parallel for trending and weekly", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const trendingKinds = mockedGetTrendingPools.mock.calls
      .map((c) => c[0])
      .sort();
    const weeklyKinds = mockedGetWeeklyTrending.mock.calls
      .map((c) => c[0])
      .sort();
    expect(trendingKinds).toEqual(["movie", "series"]);
    expect(weeklyKinds).toEqual(["movie", "series"]);
  });

  it("filtered CW empty state hides the CW row in the Movies sub-home", async () => {
    // CW has only a series entry; Movies sub-home should filter it out
    // and hide the row entirely per PRD §F-009 acceptance.
    mockedCwList.mockResolvedValue([cw("s1", "series")]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).toBeNull();
  });

  it("filtered CW with matching-kind entries renders the CW row", async () => {
    mockedCwList.mockResolvedValue([
      cw("m1", "movie"),
      cw("s1", "series"),
    ]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).not.toBeNull();
    // And only the movie CW tile is in the CW row.
    const cwTiles = Array.from(
      host.querySelectorAll<HTMLElement>(
        '[data-testid="row-continue-watching"] button[data-kind]',
      ),
    );
    expect(cwTiles.length).toBe(1);
    expect(cwTiles[0]?.dataset.kind).toBe("movie");
  });
});

describe("HomeView addon catalog rows (F-008 row 5)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    displaySettings._resetForTests();
    mockedCwList.mockReset();
    mockedGetTrendingPools.mockReset();
    mockedGetWeeklyTrending.mockReset();
    mockedListHomeCatalogs.mockReset();
    mockedCheckAvailability.mockReset();
    mockedCwList.mockResolvedValue([]);
    mockedGetTrendingPools.mockResolvedValue(pools([]));
    mockedGetWeeklyTrending.mockResolvedValue([]);
    mockedListHomeCatalogs.mockResolvedValue([]);
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: true,
        source_count: 1,
      })),
    );
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders one Row per HomeCatalog returned by listHomeCatalogs", async () => {
    mockedListHomeCatalogs.mockResolvedValue([
      catalog("cinemeta", "Cinemeta", "top", "movie", "Popular", [
        summary("imdb:tt1", "movie", "Matrix"),
        summary("imdb:tt2", "movie", "Heat"),
      ]),
      catalog("torrentio", "Torrentio", "trending", "movie", "Trending", [
        summary("imdb:tt3", "movie", "Inception"),
      ]),
    ]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const cinemetaRow = host.querySelector(
      '[data-testid="row-cat-cinemeta-top"]',
    );
    const torrentioRow = host.querySelector(
      '[data-testid="row-cat-torrentio-trending"]',
    );
    expect(cinemetaRow).not.toBeNull();
    expect(torrentioRow).not.toBeNull();
    // The label is the catalog name (not "From Your Addons").
    expect(cinemetaRow?.textContent).toContain("Popular");
    expect(torrentioRow?.textContent).toContain("Trending");
  });

  it("preserves the listHomeCatalogs ordering in the DOM", async () => {
    mockedListHomeCatalogs.mockResolvedValue([
      catalog("addon-a", "A", "a1", "movie", "A1", [
        summary("imdb:tt1", "movie", "M1"),
      ]),
      catalog("addon-a", "A", "a2", "movie", "A2", [
        summary("imdb:tt2", "movie", "M2"),
      ]),
      catalog("addon-b", "B", "b1", "movie", "B1", [
        summary("imdb:tt3", "movie", "M3"),
      ]),
    ]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const ids = Array.from(
      host.querySelectorAll<HTMLElement>('[data-testid^="row-cat-"]'),
    ).map((el) => el.dataset.testid);
    expect(ids).toEqual([
      "row-cat-addon-a-a1",
      "row-cat-addon-a-a2",
      "row-cat-addon-b-b1",
    ]);
  });

  it("forwards the active kind filter to listHomeCatalogs", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "series");
    await flushAsync();

    const kinds = mockedListHomeCatalogs.mock.calls.map((c) => c[0]);
    expect(kinds).toEqual(["series"]);
  });

  it("passes kind=null to listHomeCatalogs from the unfiltered Home", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const kinds = mockedListHomeCatalogs.mock.calls.map((c) => c[0]);
    expect(kinds).toEqual([null]);
  });

  it("renders no addon-catalog row when the resource resolves empty", async () => {
    mockedListHomeCatalogs.mockResolvedValue([]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const catRows = host.querySelectorAll<HTMLElement>(
      '[data-testid^="row-cat-"]',
    );
    expect(catRows.length).toBe(0);
  });
});

describe("HomeView Continue Watching row (F-012)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    displaySettings._resetForTests();
    mockedCwList.mockReset();
    mockedCwRemoveTitle.mockReset();
    mockedGetTrendingPools.mockReset();
    mockedGetWeeklyTrending.mockReset();
    mockedListHomeCatalogs.mockReset();
    mockedCheckAvailability.mockReset();
    mockedCwRemoveTitle.mockResolvedValue(1);
    mockedGetTrendingPools.mockResolvedValue(pools([]));
    mockedGetWeeklyTrending.mockResolvedValue([]);
    mockedListHomeCatalogs.mockResolvedValue([]);
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: true,
        source_count: 1,
      })),
    );
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders a Resume Sxx Eyy badge on a series CW tile with in-progress playback", async () => {
    mockedCwList.mockResolvedValue([cwEpisode("tt_series", 1, 3, 900, 1800)]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    // The tile becomes focused (Home claims initial focus on the
    // first CW tile), which is the only state in which the badge is
    // visible. The Tile renders the badge inside `tile-badge`.
    const badge = host.querySelector('[data-testid="tile-badge"]');
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toContain("Resume");
    expect(badge?.textContent).toContain("S01");
    expect(badge?.textContent).toContain("E03");
  });

  it("renders an Up next badge for an advanced-to-next-episode CW row", async () => {
    // PRD §F-012 advanced row: position_s = 0 (set by
    // `cw_record_position` after the previous episode completed).
    mockedCwList.mockResolvedValue([cwEpisode("tt_series", 2, 5, 0, 0)]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const badge = host.querySelector('[data-testid="tile-badge"]');
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toContain("Up next");
    expect(badge?.textContent).toContain("S02");
    expect(badge?.textContent).toContain("E05");
  });

  it("right-click on a CW tile calls cw_remove_title with the title id", async () => {
    mockedCwList.mockResolvedValueOnce([cwEpisode("tt_series", 1, 3, 900, 1800)]);
    // After the remove, refetched list is empty.
    mockedCwList.mockResolvedValue([]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    const tile = host.querySelector(
      '[data-testid="row-continue-watching"] button',
    ) as HTMLButtonElement;
    expect(tile).not.toBeNull();
    tile.dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
    );
    await flushAsync();
    expect(mockedCwRemoveTitle).toHaveBeenCalledWith("tt_series");
    // After the refetch, the CW row should be gone.
    await flushAsync();
    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).toBeNull();
  });

  it("movies on the CW row do NOT show a badge (only series get Resume/Up next labels)", async () => {
    mockedCwList.mockResolvedValue([cw("tt_movie", "movie")]);

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();

    // CW row visible (movie row exists).
    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).not.toBeNull();
    // But no badge — movies show just the title.
    const badge = host.querySelector(
      '[data-testid="row-continue-watching"] [data-testid="tile-badge"]',
    );
    expect(badge).toBeNull();
  });
});

describe("HomeView availability filter (F-006)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    displaySettings._resetForTests();
    mockedCwList.mockReset();
    mockedCwRemoveTitle.mockReset();
    mockedGetTrendingPools.mockReset();
    mockedGetWeeklyTrending.mockReset();
    mockedListHomeCatalogs.mockReset();
    mockedCheckAvailability.mockReset();
    mockedCwList.mockResolvedValue([]);
    mockedCwRemoveTitle.mockResolvedValue(0);
    mockedGetTrendingPools.mockResolvedValue(pools([]));
    mockedGetWeeklyTrending.mockResolvedValue([]);
    mockedListHomeCatalogs.mockResolvedValue([]);
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("PRD §F-006: catalog rows call check_availability with their items batched", async () => {
    mockedGetTrendingPools.mockImplementation(async () =>
      pools([
        summary("m1", "movie", "Matrix"),
        summary("m2", "movie", "Heat"),
      ]),
    );
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: true,
        source_count: 1,
      })),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    expect(mockedCheckAvailability).toHaveBeenCalled();
    const flatRequests = mockedCheckAvailability.mock.calls.flatMap((c) => c[0]);
    const ids = new Set(flatRequests.map((r) => r.title_id));
    expect(ids).toContain("m1");
    expect(ids).toContain("m2");
    // Trending Now batch is one call with both items (not split per tile).
    const sizes = mockedCheckAvailability.mock.calls.map((c) => c[0].length);
    expect(sizes.some((n) => n === 2)).toBe(true);
  });

  it("PRD §F-006: tiles whose availability resolves false hide by default", async () => {
    mockedGetTrendingPools.mockImplementation(async () =>
      pools([
        summary("m1", "movie", "Available"),
        summary("m2", "movie", "Unavailable"),
      ]),
    );
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: i.title_id === "m1",
        source_count: i.title_id === "m1" ? 1 : 0,
      })),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();
    // Two flushes to allow availability state-set to propagate.
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>(
        '[data-testid="row-trending-now"] button[data-availability]',
      ),
    );
    const ids = tiles.map((t) => {
      const tid = t.getAttribute("data-testid") ?? "";
      return tid.split("-").pop();
    });
    expect(ids).toContain("m1");
    expect(ids).not.toContain("m2");
  });

  it("PRD §F-006: when showUnavailable is ON, unavailable tiles render with the badge", async () => {
    displaySettings.setShowUnavailable(true);
    mockedGetTrendingPools.mockImplementation(async () =>
      pools([
        summary("m1", "movie", "Available"),
        summary("m2", "movie", "Unavailable"),
      ]),
    );
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: i.title_id === "m1",
        source_count: i.title_id === "m1" ? 1 : 0,
      })),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>(
        '[data-testid="row-trending-now"] button[data-availability]',
      ),
    );
    expect(tiles.length).toBe(2);
    expect(
      host.querySelectorAll('[data-testid="tile-no-source-badge"]').length,
    ).toBe(1);

    displaySettings.setShowUnavailable(false);
  });

  it("PRD §F-006: tiles render skeleton while check_availability is pending", async () => {
    mockedGetTrendingPools.mockImplementation(async () =>
      pools([summary("m1", "movie", "Pending")]),
    );
    // Hang the availability check so it never resolves during the test.
    mockedCheckAvailability.mockImplementation(
      () =>
        new Promise(() => {
          /* never resolves */
        }),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();

    // The single trending tile is in pending state — skeleton present,
    // poster absent, aria-busy true.
    const skeletons = host.querySelectorAll('[data-testid="tile-skeleton"]');
    expect(skeletons.length).toBeGreaterThanOrEqual(1);
  });

  it("PRD §F-006: Continue Watching tiles are NOT availability-filtered", async () => {
    // CW shows a series the user has been watching; even if no enabled
    // addon currently has a stream (transient), the tile must remain
    // visible so the user can manually remove it.
    mockedCwList.mockResolvedValue([cw("cw1", "movie")]);
    mockedCheckAvailability.mockImplementation(async (items) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: false, // every CW item is "unavailable" per the mock
        source_count: 0,
      })),
    );

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, null);
    await flushAsync();
    await flushAsync();

    // CW row still visible with the tile.
    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).not.toBeNull();
    const cwTiles = host.querySelectorAll(
      '[data-testid="row-continue-watching"] button',
    );
    expect(cwTiles.length).toBe(1);
    // And check_availability was NOT called for the CW item (PRD lists
    // trending / sub-homes / search / addon catalogs, NOT CW).
    const allReqIds = mockedCheckAvailability.mock.calls
      .flatMap((c) => c[0])
      .map((r) => r.title_id);
    expect(allReqIds).not.toContain("cw1");
  });

  it("PRD §F-006: network error treats every tile as available so the row doesn't go dark", async () => {
    mockedGetTrendingPools.mockImplementation(async () =>
      pools([summary("m1", "movie", "Matrix")]),
    );
    mockedCheckAvailability.mockRejectedValue(new Error("network down"));

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "movie");
    await flushAsync();
    await flushAsync();

    const tiles = Array.from(
      host.querySelectorAll<HTMLElement>(
        '[data-testid="row-trending-now"] button[data-availability]',
      ),
    );
    expect(tiles.length).toBe(1);
    expect(tiles[0]?.getAttribute("data-availability")).toBe("available");
  });
});

describe("interleaveByKind", () => {
  it("alternates equal-length lists", () => {
    expect(interleaveByKind([1, 2, 3], [10, 20, 30])).toEqual([
      1, 10, 2, 20, 3, 30,
    ]);
  });

  it("drops missing slots from the shorter list", () => {
    expect(interleaveByKind([1, 2], [10, 20, 30, 40])).toEqual([
      1, 10, 2, 20, 30, 40,
    ]);
  });

  it("returns the other list when one input is empty", () => {
    expect(interleaveByKind<number>([], [1, 2, 3])).toEqual([1, 2, 3]);
    expect(interleaveByKind<number>([1, 2, 3], [])).toEqual([1, 2, 3]);
  });

  it("returns an empty list when both inputs are empty", () => {
    expect(interleaveByKind<number>([], [])).toEqual([]);
  });
});
