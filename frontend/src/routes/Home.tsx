// Home screen. PRD §F-008:
//
//   1. Continue Watching (hidden if empty)
//   2. Trending Now (F-004 top pool)
//   3. Hidden Gems (F-004 gems pool)
//   4. Trending This Week (TMDB-only /trending/{type}/week)
//   5. Catalogs from installed addons (deferred — see ADR-068; lands
//      in a follow-up session)
//
// The Home route accepts no kind filter and renders a mixed
// movies + series feed (PRD §F-009: "Movies and Series sub-homes are
// identical structure to Home, filtered to type=movie / type=series" —
// which positions Home as the unfiltered superset). The trending and
// weekly aggregators in `kino-metadata` are per-kind (PRD §F-004), so
// for `kind === null` HomeView fires both calls in parallel and
// interleaves them via [`interleaveByKind`] — alternating movie /
// series at each index — to produce a balanced mixed row. CW is
// already kind-tagged so the `kind === null` path simply does not
// filter it.
//
// `HomeView` is parameterized by `kind` so `Movies.tsx` and
// `Series.tsx` (F-009 sub-homes) can mount the same row stack with
// the per-kind filter applied. Switching between Home / Movies /
// Series via the nav rail re-renders only the route content (the
// shared Shell in `App.tsx` stays mounted), satisfying F-009's
// "instant — no full reload" acceptance.

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
 * filter; `null` means Home (unfiltered — mixed movies + series).
 */
export type HomeViewProps = {
  /**
   * `"movie"` → render the Movies sub-home variant (PRD §F-009).
   * `"series"` → render the Series sub-home variant.
   * `null` → render the unfiltered Home: trending and weekly pools
   * for both kinds, interleaved.
   */
  kind: TitleKind | null;
};

/**
 * Alternate two per-kind lists into one mixed list at index granularity:
 * `[a0, b0, a1, b1, a2, b2, ...]`, dropping `undefined` slots when one
 * list is shorter. Used by the unfiltered Home to balance movies and
 * series across the trending and weekly rows so neither kind dominates
 * the visible window. Dedup is intentionally a no-op: the inputs come
 * from disjoint per-kind feeds, so collisions imply distinct titles
 * with shared ids across kinds (rare; the `kind:id` pair is the natural
 * key) — letting them through keeps the row order predictable.
 */
export function interleaveByKind<T>(a: readonly T[], b: readonly T[]): T[] {
  const out: T[] = [];
  const max = Math.max(a.length, b.length);
  for (let i = 0; i < max; i++) {
    const ai = a[i];
    if (ai !== undefined) out.push(ai);
    const bi = b[i];
    if (bi !== undefined) out.push(bi);
  }
  return out;
}

export const HomeView: Component<HomeViewProps> = (props) => {
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

  const [poolsResource] = createResource<
    TrendingPools,
    [TitleKind | null, string]
  >(
    () => [props.kind, locale()] as [TitleKind | null, string],
    async ([kind, loc]) => {
      if (!hasTauri()) return { top_trending: [], hidden_gems: [] };
      try {
        if (kind === null) {
          const [m, s] = await Promise.all([
            getTrendingPools("movie", loc),
            getTrendingPools("series", loc),
          ]);
          return {
            top_trending: interleaveByKind(m.top_trending, s.top_trending),
            hidden_gems: interleaveByKind(m.hidden_gems, s.hidden_gems),
          };
        }
        return await getTrendingPools(kind, loc);
      } catch (e) {
        console.warn("get_trending_pools failed", e);
        return { top_trending: [], hidden_gems: [] };
      }
    },
  );

  const [weeklyResource] = createResource<
    TitleSummary[],
    [TitleKind | null, string]
  >(
    () => [props.kind, locale()] as [TitleKind | null, string],
    async ([kind, loc]) => {
      if (!hasTauri()) return [];
      try {
        if (kind === null) {
          const [m, s] = await Promise.all([
            getWeeklyTrending("movie", loc),
            getWeeklyTrending("series", loc),
          ]);
          return interleaveByKind(m, s);
        }
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
