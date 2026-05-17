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
    getTrendingPools: vi.fn(),
    getWeeklyTrending: vi.fn(),
  };
});

const tauri = await import("../lib/tauri");
const mockedCwList = vi.mocked(tauri.cwList);
const mockedGetTrendingPools = vi.mocked(tauri.getTrendingPools);
const mockedGetWeeklyTrending = vi.mocked(tauri.getWeeklyTrending);

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
    mockedCwList.mockReset();
    mockedGetTrendingPools.mockReset();
    mockedGetWeeklyTrending.mockReset();
    mockedCwList.mockResolvedValue([]);
    mockedGetTrendingPools.mockResolvedValue(pools([]));
    mockedGetWeeklyTrending.mockResolvedValue([]);
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
