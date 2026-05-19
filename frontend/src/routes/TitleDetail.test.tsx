// PRD §F-010 title detail view tests. The four code-acceptance items
// the suite covers:
//
//   1. Resume button only present when matching CW entry exists
//   2. Stream parsing badges from §8 fixtures (re-verified through the
//      IPC shape — the regex set itself is tested in
//      `kino_addons::parse`; this asserts the wiring)
//   3. Episode list shows progress for partially-watched episodes
//   4. Back navigation returns focus to the originating tile
//
// Plus the supporting UI rendering: metadata chips, ratings row,
// summary, cast row, season selector, on-screen Back button.

import { render } from "solid-js/web";
import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TitleDetailRoute } from "./TitleDetail";
import { Home } from "./Home";
import {
  _resetForTests as _resetFocus,
  _returnFocusStackForTests,
  focusedId,
  pushReturnFocus,
} from "../input/focus";
import { _resetForTests as _resetProfile } from "../input/profile";
import type {
  Artwork,
  CastMember,
  Episode,
  StreamRow,
  TitleDetail as TitleDetailData,
  TitleKind,
} from "../lib/tauri";

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return {
    ...actual,
    hasTauri: () => true,
    getTitleDetail: vi.fn(),
    getStreams: vi.fn(),
    resolveArtwork: vi.fn(),
    cwUpsert: vi.fn(async () => undefined),
    cwRecordPosition: vi.fn(async (entry) => entry),
    cwList: vi.fn(async () => []),
    getTrendingPools: vi.fn(async () => ({
      top_trending: [],
      hidden_gems: [],
    })),
    getWeeklyTrending: vi.fn(async () => []),
    listHomeCatalogs: vi.fn(async () => []),
    // PRD §F-006: HomeView fires `check_availability` per row on mount;
    // the title-detail tests transit through Home for one navigation
    // case, so stub the call to keep the warn-log clean.
    checkAvailability: vi.fn(async (items: { title_id: string; type: "movie" | "series" }[]) =>
      items.map((i) => ({
        title_id: i.title_id,
        type: i.type,
        available: true,
        source_count: 1,
      })),
    ),
  };
});

const tauri = await import("../lib/tauri");
const mockedGetTitleDetail = vi.mocked(tauri.getTitleDetail);
const mockedGetStreams = vi.mocked(tauri.getStreams);
const mockedResolveArtwork = vi.mocked(tauri.resolveArtwork);

function emptyArtwork(): Artwork {
  return {
    poster: "",
    backdrop: "",
    logo: "",
    clearart: "",
    summary: "",
    sources: {
      poster: "placeholder",
      backdrop: "placeholder",
      logo: "placeholder",
      clearart: "placeholder",
      summary: "placeholder",
    },
  };
}

function detail(overrides: Partial<TitleDetailData> = {}): TitleDetailData {
  return {
    id: "imdb:tt0133093",
    kind: "movie" as TitleKind,
    title: "The Matrix",
    year: 1999,
    runtime_minutes: 136,
    age_rating: "R",
    genres: ["Action", "Sci-Fi"],
    summary: "A computer hacker learns about reality.",
    imdb_rating: 8.7,
    tmdb_rating: 8.2,
    trakt_rating: 8.5,
    backdrop: "https://example.test/bg.jpg",
    logo: null,
    poster: "https://example.test/poster.jpg",
    cast: [],
    episodes: [],
    resume_position_s: null,
    resume_duration_s: null,
    resume_season: null,
    resume_episode: null,
    resume_video_id: null,
    stremio_id: "tt0133093",
    ...overrides,
  };
}

function cast(name: string, character: string | null = null, photo: string | null = null): CastMember {
  return { name, character, photo };
}

function episode(season: number, ep: number, progress = 0): Episode {
  return {
    video_id: `tt0944947:${season}:${ep}`,
    season,
    episode: ep,
    title: `S${season}E${ep}`,
    air_date: "2011-04-17",
    overview: `Episode ${ep} synopsis`,
    thumbnail: `https://example.test/t${season}-${ep}.jpg`,
    progress,
  };
}

function stream(
  overrides: Partial<StreamRow> = {},
): StreamRow {
  return {
    addon_id: "torrentio",
    addon_name: "Torrentio",
    name: "Torrentio",
    detail: "The Matrix 1999 2160p UHD BluRay HEVC TrueHD Atmos 7.1",
    quality: "4K",
    hdr: null,
    audio: "ATMOS",
    codec: "H265",
    seeders: 156,
    size_bytes: 25_125_762_662,
    url: null,
    info_hash: "deadbeef",
    file_idx: null,
    sources: [],
    ...overrides,
  };
}

async function flush() {
  // Two microtask drains let the createResource fetcher resolve and the
  // downstream createEffect commits run.
  await Promise.resolve();
  await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
  await Promise.resolve();
}

function mountDetail(host: HTMLElement, id = "imdb%3Att0133093", search = "?kind=movie") {
  const history = createMemoryHistory();
  history.set({ value: `/title/${id}${search}` });
  return render(
    () => (
      <MemoryRouter history={history}>
        <Route path="/title/:id" component={TitleDetailRoute} />
      </MemoryRouter>
    ),
    host,
  );
}

describe("TitleDetailRoute (F-010)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    mockedGetTitleDetail.mockReset();
    mockedGetStreams.mockReset();
    mockedResolveArtwork.mockReset();
    mockedGetTitleDetail.mockResolvedValue(detail());
    mockedGetStreams.mockResolvedValue([]);
    mockedResolveArtwork.mockResolvedValue(emptyArtwork());
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders title, year, runtime, age rating, and genres", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const chips = host.querySelector('[data-testid="detail-metadata-chips"]');
    expect(chips).not.toBeNull();
    expect(chips!.textContent).toContain("1999");
    expect(chips!.textContent).toContain("136");
    expect(chips!.textContent).toContain("R");
    expect(chips!.textContent).toContain("Action");
    expect(chips!.textContent).toContain("Sci-Fi");
  });

  it("renders all three ratings when known", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(host.querySelector('[data-testid="detail-rating-imdb"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="detail-rating-tmdb"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="detail-rating-trakt"]')).not.toBeNull();
  });

  it("hides individual ratings when their value is null (PRD F-010 'only when known')", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({ tmdb_rating: null, trakt_rating: null }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(host.querySelector('[data-testid="detail-rating-imdb"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="detail-rating-tmdb"]')).toBeNull();
    expect(host.querySelector('[data-testid="detail-rating-trakt"]')).toBeNull();
  });

  it("renders the summary in the user's primary language", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const summary = host.querySelector('[data-testid="detail-summary"]');
    expect(summary).not.toBeNull();
    expect(summary!.textContent).toContain("A computer hacker");
  });

  it("renders the cast row with photo for each member", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({
        cast: [
          cast("Keanu Reeves", "Neo", "https://example.test/k.jpg"),
          cast("Carrie-Anne Moss", "Trinity", null),
          cast("Laurence Fishburne", "Morpheus", "https://example.test/l.jpg"),
        ],
      }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const row = host.querySelector('[data-testid="detail-cast-row"]');
    expect(row).not.toBeNull();
    expect(row!.textContent).toContain("Keanu Reeves");
    expect(row!.textContent).toContain("Carrie-Anne Moss");
    expect(row!.textContent).toContain("Morpheus");
  });

  // PRD §F-010 §6A: Resume button only present when matching CW entry exists.
  it("shows the Play button (not Resume) when no CW entry exists", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(host.querySelector('[data-testid="detail-play-button"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="detail-resume-button"]')).toBeNull();
  });

  it("shows the Resume button (not Play) when a CW entry exists", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({
        resume_position_s: 1800,
        resume_duration_s: 8160,
        resume_video_id: "tt0133093",
      }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(host.querySelector('[data-testid="detail-resume-button"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="detail-play-button"]')).toBeNull();
  });

  // PRD §F-010 §6A: Stream parsing produces correct badges from fixture
  // filenames (the wiring; the regex set itself is tested in Rust).
  it("renders quality / HDR / audio / codec / seeders / size badges per stream", async () => {
    mockedGetStreams.mockResolvedValue([
      stream({
        quality: "4K",
        hdr: null,
        audio: "ATMOS",
        codec: "H265",
        seeders: 156,
        size_bytes: 25_125_762_662,
        info_hash: "h1",
      }),
      stream({
        quality: "1080p",
        hdr: "DV",
        audio: "DTSHD",
        codec: "H265",
        seeders: 42,
        size_bytes: 12_884_901_888,
        info_hash: "h2",
      }),
    ]);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const rows = host.querySelectorAll('[data-testid^="detail-stream-"]');
    // We render only the visible rows under the streams section; the
    // first-letter prefix ("detail-stream-") matches both <li> wrappers
    // and inner badges in the loading state. Walk the rows that have a
    // numeric index suffix.
    const numericRows = Array.from(rows).filter((el) =>
      /^detail-stream-\d+$/.test(el.getAttribute("data-testid") ?? ""),
    );
    expect(numericRows.length).toBe(2);
    const html = host.innerHTML;
    expect(html).toContain("4K");
    expect(html).toContain("1080p");
    expect(html).toContain("ATMOS");
    expect(html).toContain("DTSHD");
    expect(html).toContain("DV");
    expect(html).toContain("H265");
    expect(host.querySelectorAll('[data-testid="badge-quality"]').length).toBe(
      2,
    );
    expect(host.querySelectorAll('[data-testid="badge-audio"]').length).toBe(2);
    expect(host.querySelectorAll('[data-testid="badge-codec"]').length).toBe(2);
    expect(host.querySelectorAll('[data-testid="badge-hdr"]').length).toBe(1);
    expect(host.querySelectorAll('[data-testid="badge-seeders"]').length).toBe(
      2,
    );
    expect(host.querySelectorAll('[data-testid="badge-size"]').length).toBe(2);
  });

  it("shows the empty-state message when no streams are returned", async () => {
    mockedGetStreams.mockResolvedValue([]);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(
      host.querySelector('[data-testid="detail-streams-empty"]'),
    ).not.toBeNull();
  });

  // PRD §F-010 §6A: episode list shows correct progress for partially-
  // watched episodes.
  it("renders series season selector + episode list with progress bars", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({
        kind: "series",
        id: "imdb:tt0944947",
        stremio_id: "tt0944947",
        title: "Game of Thrones",
        episodes: [
          episode(1, 1, 0.5), // 50% watched
          episode(1, 2, 0),
          episode(1, 3, 0),
          episode(2, 1, 0),
        ],
      }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host, "imdb%3Att0944947", "?kind=series");
    await flush();

    // Season buttons: S1, S2
    expect(
      host.querySelector('[data-testid="detail-season-button-1"]'),
    ).not.toBeNull();
    expect(
      host.querySelector('[data-testid="detail-season-button-2"]'),
    ).not.toBeNull();
    // Episode list for season 1: 3 episodes (S1E1/2/3).
    expect(
      host.querySelectorAll('[data-testid^="detail-episode-1-"]').length,
    ).toBeGreaterThanOrEqual(3);
    // Progress bar reads 0.5 for S1E1.
    const progress = host.querySelector(
      '[data-testid="detail-episode-progress-1-1"]',
    );
    expect(progress).not.toBeNull();
    expect(progress!.getAttribute("data-progress")).toBe("0.500");
    // S1E2 progress is 0.
    const progress2 = host.querySelector(
      '[data-testid="detail-episode-progress-1-2"]',
    );
    expect(progress2!.getAttribute("data-progress")).toBe("0.000");
  });

  it("switches the visible episode list when a different season is clicked", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({
        kind: "series",
        episodes: [episode(1, 1, 0), episode(2, 1, 0), episode(2, 2, 0)],
      }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host, "imdb%3Att0944947", "?kind=series");
    await flush();
    // Initially season 1 is selected.
    expect(
      host.querySelectorAll('[data-testid^="detail-episode-1-"]').length,
    ).toBe(1);
    // Click season 2.
    const s2 = host.querySelector<HTMLButtonElement>(
      '[data-testid="detail-season-button-2"]',
    );
    expect(s2).not.toBeNull();
    s2!.click();
    await flush();
    expect(
      host.querySelectorAll('[data-testid^="detail-episode-2-"]').length,
    ).toBe(2);
    expect(
      host.querySelectorAll('[data-testid^="detail-episode-1-"]').length,
    ).toBe(0);
  });

  it("forwards (season, episode) to get_streams when clicking an episode", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({
        kind: "series",
        stremio_id: "tt0944947",
        episodes: [episode(1, 1, 0), episode(1, 2, 0)],
      }),
    );
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host, "imdb%3Att0944947", "?kind=series");
    await flush();
    // First get_streams call is for the default selection (S1E1).
    expect(mockedGetStreams).toHaveBeenCalled();
    const firstCall = mockedGetStreams.mock.calls[0]!;
    expect(firstCall[2]).toBe(1); // season
    expect(firstCall[3]).toBe(1); // episode
    // Click S1E2.
    const e2 = host.querySelector<HTMLButtonElement>(
      '[data-testid="detail-episode-1-2"] button',
    );
    e2!.click();
    await flush();
    const lastCall =
      mockedGetStreams.mock.calls[mockedGetStreams.mock.calls.length - 1]!;
    expect(lastCall[2]).toBe(1);
    expect(lastCall[3]).toBe(2);
  });

  // PRD §F-010 §6A: back navigation returns focus to the originating tile.
  it("pops the return-focus stack and restores focus on Back click", async () => {
    pushReturnFocus("row-trending-now-imdb:tt0133093");
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(_returnFocusStackForTests().length).toBe(1);
    const back = host.querySelector<HTMLButtonElement>(
      '[data-testid="detail-back-button"]',
    );
    expect(back).not.toBeNull();
    back!.click();
    // Pop happens synchronously in goBack(). The focus restore is
    // deferred via queueMicrotask but the stack pop is immediate.
    expect(_returnFocusStackForTests().length).toBe(0);
  });

  it("invokes resolveArtwork in parallel with getTitleDetail", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    expect(mockedGetTitleDetail).toHaveBeenCalledTimes(1);
    expect(mockedResolveArtwork).toHaveBeenCalledTimes(1);
  });

  it("forwards the URL kind=series to getTitleDetail for series ids", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host, "imdb%3Att0944947", "?kind=series");
    await flush();
    expect(mockedGetTitleDetail).toHaveBeenCalledWith(
      "imdb:tt0944947",
      "series",
      expect.any(Array),
    );
  });

  it("clicking Mark Watched posts a CW row at duration position via cw_record_position (F-012)", async () => {
    mockedGetTitleDetail.mockResolvedValue(
      detail({ runtime_minutes: 100, stremio_id: "tt0133093" }),
    );
    const cwRecordMock = vi.mocked(tauri.cwRecordPosition);
    cwRecordMock.mockClear();
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const mark = host.querySelector<HTMLButtonElement>(
      '[data-testid="detail-mark-watched-button"]',
    );
    mark!.click();
    await flush();
    // Mark Watched routes through the F-012 canonical writer so the
    // locked completion + next-episode rules apply server-side.
    expect(cwRecordMock).toHaveBeenCalledTimes(1);
    const [entry, episodes] = cwRecordMock.mock.calls[0]!;
    expect(entry.title_id).toBe("tt0133093");
    expect(entry.position_s).toBe(6000); // 100 min * 60 s
    expect(entry.duration_s).toBe(6000);
    // Movie → empty episode list (next-episode rule does not apply).
    expect(episodes).toEqual([]);
  });

  it("Mark Watched on a series passes the episode list so cw_record_position can advance to next (F-012)", async () => {
    mockedGetTitleDetail.mockResolvedValue({
      ...detail({ runtime_minutes: 60, stremio_id: "tt0944947" }),
      kind: "series",
      episodes: [
        {
          video_id: "tt0944947:1:1",
          season: 1,
          episode: 1,
          title: "Episode 1",
          air_date: null,
          overview: null,
          thumbnail: null,
          progress: 0,
        },
        {
          video_id: "tt0944947:1:2",
          season: 1,
          episode: 2,
          title: "Episode 2",
          air_date: null,
          overview: null,
          thumbnail: null,
          progress: 0,
        },
      ],
    });
    const cwRecordMock = vi.mocked(tauri.cwRecordPosition);
    cwRecordMock.mockClear();
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mountDetail(host);
    await flush();
    const mark = host.querySelector<HTMLButtonElement>(
      '[data-testid="detail-mark-watched-button"]',
    );
    mark!.click();
    await flush();
    expect(cwRecordMock).toHaveBeenCalledTimes(1);
    const [, episodes] = cwRecordMock.mock.calls[0]!;
    expect(episodes).toEqual([
      [1, 1],
      [1, 2],
    ]);
  });
});

// PRD §F-010 §6A: separate test for tile-click → detail navigation
// pushing the originating focus id onto the return stack, exercised
// through the Home route's onActivate handler.
describe("Home → detail navigation", () => {
  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    mockedGetTitleDetail.mockReset();
    mockedGetStreams.mockReset();
    mockedResolveArtwork.mockReset();
    mockedGetTitleDetail.mockResolvedValue(detail());
    mockedGetStreams.mockResolvedValue([]);
    mockedResolveArtwork.mockResolvedValue(emptyArtwork());
  });

  it("Home tile activation pushes the focused id onto the return stack", async () => {
    const mockedTrendingPools = vi.mocked(tauri.getTrendingPools);
    mockedTrendingPools.mockResolvedValue({
      top_trending: [
        {
          id: "imdb:tt0133093",
          kind: "movie",
          title: "The Matrix",
          year: 1999,
          poster: null,
          rating: null,
        },
      ],
      hidden_gems: [],
    });
    const host = document.createElement("div");
    document.body.appendChild(host);
    let dispose: (() => void) | null = null;
    try {
      dispose = render(
        () => (
          <MemoryRouter>
            <Route path="/" component={Home} />
            <Route path="/title/:id" component={TitleDetailRoute} />
          </MemoryRouter>
        ),
        host,
      );
      await flush();
      const tile = host.querySelector<HTMLButtonElement>(
        '[data-testid="tile-row-trending-now-imdb:tt0133093"]',
      );
      expect(tile).not.toBeNull();
      // Sanity: the tile was registered with the focus manager.
      expect(focusedId()).toBe("row-trending-now-imdb:tt0133093");
      tile!.click();
      // pushReturnFocus was called synchronously inside the click
      // handler before navigate; the stack now holds the originating id.
      expect(_returnFocusStackForTests()).toEqual([
        "row-trending-now-imdb:tt0133093",
      ]);
    } finally {
      dispose?.();
      host.remove();
    }
  });
});
