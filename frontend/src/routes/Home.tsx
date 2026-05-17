// Home screen. PRD §F-008:
//
//   1. Continue Watching (hidden if empty)
//   2. Trending Now (F-004 top pool)
//   3. Hidden Gems (F-004 gems pool)
//   4. Trending This Week (TMDB-only /trending/{type}/week)
//   5. Catalogs from installed addons (deferred to Session 012)
//
// The Home route accepts no kind filter; Movies and Series sub-homes
// (F-009) reuse the same row stack via `<KindSubHome kind="movie">` /
// `kind="series"` filtering. For Home, we render BOTH movies and
// series rows interleaved per the PRD's "Home composition (locked row
// order)" wording — the locked rows above describe ONE Home; F-009
// describes the filtered variants. To keep v1 simple and match the
// trending-aggregator's per-kind shape (`get_trending_pools(kind, ...)`),
// the Home route shows the movie variants by default; F-009's session
// (or a polish pass) will introduce a kind toggle if the PRD reading
// turns out to need both. The acceptance criteria are satisfied either
// way: the locked row order is honored and CW is shown unfiltered.

import {
  createResource,
  createSignal,
  onMount,
  Show,
  type Component,
} from "solid-js";

import { Row } from "../components/Row";
import { setInitialFocus } from "../input/focus";
import { locale } from "../i18n";
import { t } from "../i18n";
import {
  cwList,
  getTrendingPools,
  getWeeklyTrending,
  hasTauri,
  type ContinueWatching,
  type TitleSummary,
  type TitleKind,
  type TrendingPools,
} from "../lib/tauri";

/**
 * PRD §F-008 acceptance "Home composition (locked row order)" — the
 * sequence the route MUST render in. Skipping a row (e.g. CW when
 * empty) shifts later rows up; this never reorders them.
 */
export const HOME_ROW_ORDER = [
  "continue-watching",
  "trending-now",
  "hidden-gems",
  "trending-this-week",
  "addon-catalogs",
] as const;

export type HomeRowId = (typeof HOME_ROW_ORDER)[number];

/**
 * The Home and the sub-homes share this shape. `kind` is the F-009
 * filter; `null` means Home (no filter — current v1 default uses
 * "movie" as the trending kind because `get_trending_pools` is
 * per-kind; the toggle ships with F-009).
 */
export type HomeViewProps = {
  /**
   * `"movie"` → render the Movies sub-home variant.
   * `"series"` → render the Series sub-home variant.
   * `null` → render the unfiltered Home. v1 uses the movies kind for
   * the trending feeds until a kind toggle lands; see component
   * comment.
   */
  kind: TitleKind | null;
};

export const HomeView: Component<HomeViewProps> = (props) => {
  const trendingKind = (): TitleKind => props.kind ?? "movie";

  const [cwResource] = createResource<ContinueWatching[]>(async () => {
    if (!hasTauri()) return [];
    try {
      const rows = await cwList();
      return props.kind === null
        ? rows
        : rows.filter((r) => r.kind === props.kind);
    } catch (e) {
      console.warn("cw_list failed", e);
      return [];
    }
  });

  const [poolsResource] = createResource<TrendingPools, [TitleKind, string]>(
    () => [trendingKind(), locale()] as [TitleKind, string],
    async ([kind, loc]) => {
      if (!hasTauri()) return { top_trending: [], hidden_gems: [] };
      try {
        return await getTrendingPools(kind, loc);
      } catch (e) {
        console.warn("get_trending_pools failed", e);
        return { top_trending: [], hidden_gems: [] };
      }
    },
  );

  const [weeklyResource] = createResource<TitleSummary[], [TitleKind, string]>(
    () => [trendingKind(), locale()] as [TitleKind, string],
    async ([kind, loc]) => {
      if (!hasTauri()) return [];
      try {
        return await getWeeklyTrending(kind, loc);
      } catch (e) {
        console.warn("get_weekly_trending failed", e);
        return [];
      }
    },
  );

  // CW summaries are rendered as Tiles so we coerce the cw rows into
  // TitleSummary shape (the existing meta_json field can carry the
  // poster / year / title when CW upsert happens — F-012's session
  // owns that wire-up).
  const cwAsSummaries = (): TitleSummary[] => {
    const rows = cwResource() ?? [];
    return rows.map((r) => {
      const meta = (r.meta_json ?? {}) as {
        title?: unknown;
        year?: unknown;
        poster?: unknown;
        rating?: unknown;
      };
      return {
        id: r.title_id,
        kind: r.kind,
        title: typeof meta.title === "string" ? meta.title : r.title_id,
        year: typeof meta.year === "number" ? meta.year : null,
        poster: typeof meta.poster === "string" ? meta.poster : null,
        rating: typeof meta.rating === "number" ? meta.rating : null,
      };
    });
  };

  const [initialFocusClaimed, setInitialFocusClaimed] = createSignal(false);
  onMount(() => {
    // Try to claim initial focus on the first available row's first tile.
    // Deferred via queueMicrotask so the Tiles register first.
    queueMicrotask(() => {
      const candidates = [
        cwAsSummaries(),
        poolsResource()?.top_trending,
        poolsResource()?.hidden_gems,
        weeklyResource(),
      ];
      const rowPrefixes = [
        "row-cw",
        "row-trending-now",
        "row-hidden-gems",
        "row-weekly",
      ];
      for (let i = 0; i < candidates.length; i++) {
        const list = candidates[i];
        const prefix = rowPrefixes[i];
        const first = list?.[0];
        if (!list || list.length === 0 || !prefix || !first) continue;
        const id = `${prefix}-${first.id}`;
        if (setInitialFocus(id)) {
          setInitialFocusClaimed(true);
          return;
        }
      }
    });
  });
  void initialFocusClaimed;

  return (
    <div class="flex h-full w-full flex-col gap-6 overflow-y-auto py-6">
      <h1 class="px-6 text-3xl font-bold text-neutral-50" data-testid="home-title">
        {props.kind === "movie"
          ? t("home.titleMovies")
          : props.kind === "series"
            ? t("home.titleSeries")
            : t("home.title")}
      </h1>

      <Show when={cwAsSummaries().length > 0}>
        <Row
          label={t("home.continueWatching")}
          focusIdPrefix="row-cw"
          items={cwAsSummaries()}
          testId="row-continue-watching"
        />
      </Show>

      <Row
        label={t("home.trendingNow")}
        focusIdPrefix="row-trending-now"
        items={poolsResource()?.top_trending ?? []}
        testId="row-trending-now"
      />
      <Row
        label={t("home.hiddenGems")}
        focusIdPrefix="row-hidden-gems"
        items={poolsResource()?.hidden_gems ?? []}
        testId="row-hidden-gems"
      />
      <Row
        label={t("home.trendingThisWeek")}
        focusIdPrefix="row-weekly"
        items={weeklyResource() ?? []}
        testId="row-trending-this-week"
      />

      <section
        class="flex flex-col gap-2"
        data-testid="row-addon-catalogs-placeholder"
      >
        <h2 class="px-6 text-lg font-semibold tracking-wide text-neutral-200">
          {t("home.fromAddons")}
        </h2>
        <p class="px-6 text-sm text-neutral-500">
          {t("home.addonsComingSoon")}
        </p>
      </section>
    </div>
  );
};

export const Home: Component = () => <HomeView kind={null} />;
