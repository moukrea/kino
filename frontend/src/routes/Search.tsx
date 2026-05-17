// F-011 Search route.
//
// Layout:
//
//   - Top-of-screen search input (PRD §F-011: "Top-of-screen search
//     bar, focused on entry"). Native `<input>` so the focus + IME
//     stack is the browser's, not the F-017 focus manager's.
//   - Empty query → "Recent searches" surface, last 10 entries from
//     `recent_searches` (PRD §F-011: "Empty query: 'Recent searches'").
//   - Non-empty query → debounced live search (300ms; PRD §8
//     `SEARCH_DEBOUNCE_MS`), result tiles in a flexed grid. Page size
//     is 20 (PRD §8 `SEARCH_PAGE_SIZE`); a "Load more" button surfaces
//     when the backend reports `has_more = true`.
//   - IMDb-id shortcut (PRD §F-011 acceptance): if the backend resolves
//     `^tt\d+$` via TMDB `/find`, the response's `direct` field carries
//     the resolved `(id, kind)` and we navigate to `/title/:id?kind=`
//     immediately rather than render a result list.
//   - F-006 availability filter runs server-side; the frontend just
//     renders whatever the backend returns.
//
// Focus / shortcuts:
//
//   - PRD §F-011: "/ keyboard shortcut focuses search on Linux. Y
//     button on gamepad focuses search from anywhere." The global
//     navigation is wired in `App.tsx` via `onAction("search", ...)`;
//     this route's `onMount` autofocuses the input so the user can
//     start typing as soon as the route renders.
//   - Recent-search tiles and result tiles use the F-017 `<Focusable>`
//     wrapper so D-pad / arrow / gamepad traversal sees them. A "Clear
//     history" button is also focusable, scoped to the recent-searches
//     surface.

import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";
import { useNavigate } from "@solidjs/router";

import { Focusable } from "../components/Focusable";
import { Tile } from "../components/Tile";
import { focusedId, pushReturnFocus, setFocusedId } from "../input/focus";
import { locale, t } from "../i18n";
import {
  hasTauri,
  recentSearchesClear,
  recentSearchesList,
  search,
  type SearchResponse,
  type TitleSummary,
} from "../lib/tauri";

/**
 * PRD §8 `SEARCH_DEBOUNCE_MS`. Exported for tests so debounce timing
 * is asserted against the same constant the production code uses.
 */
export const SEARCH_DEBOUNCE_MS = 300;

/**
 * Stable id of the `<input>` Focusable so the global `search` action
 * (`/` on keyboard, Y on gamepad — wired in `App.tsx`) can refocus
 * the box from any route.
 */
export const SEARCH_INPUT_FOCUS_ID = "search-input";

/**
 * `data-testid` attached to the rendered `<input>`. Exported so the
 * App.tsx global-search handler can find and focus it without
 * duplicating the literal.
 */
export const SEARCH_INPUT_TEST_ID = "search-input";

export const Search: Component = () => {
  const navigate = useNavigate();

  // Live input value.
  const [query, setQuery] = createSignal("");
  // Debounced value that triggers the actual fetch.
  const [activeQuery, setActiveQuery] = createSignal("");
  const [page, setPage] = createSignal(1);
  const [results, setResults] = createSignal<TitleSummary[]>([]);
  const [hasMore, setHasMore] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  let debounceHandle: ReturnType<typeof setTimeout> | null = null;
  let pendingSearchSeq = 0;
  let inputEl: HTMLInputElement | undefined;

  // Recent searches resource — refetches on demand after a successful
  // search / clear.
  const [recent, { refetch: refetchRecent }] = createResource<string[]>(
    async () => {
      if (!hasTauri()) return [];
      try {
        return await recentSearchesList();
      } catch (e) {
        console.warn("recent_searches_list failed", e);
        return [];
      }
    },
  );

  const handleInput = (raw: string) => {
    setQuery(raw);
    setError(null);
    // PRD §F-011: 300ms debounce. Cancel any pending timer; restart.
    if (debounceHandle !== null) clearTimeout(debounceHandle);
    debounceHandle = setTimeout(() => {
      debounceHandle = null;
      const trimmed = raw.trim();
      setActiveQuery(trimmed);
      setPage(1);
    }, SEARCH_DEBOUNCE_MS);
  };

  /**
   * Apply a search response to component state. Direct matches navigate
   * away; non-direct responses replace the result list (page 1) OR
   * append to it (subsequent pages).
   */
  const applyResponse = (resp: SearchResponse, append: boolean) => {
    if (resp.direct !== null) {
      navigate(
        `/title/${encodeURIComponent(resp.direct.id)}?kind=${resp.direct.kind}`,
      );
      return;
    }
    setResults((prev) => (append ? [...prev, ...resp.results] : resp.results));
    setHasMore(resp.has_more);
  };

  /**
   * Fire a search call. `seq` is captured so concurrent calls
   * (rapid typing, double "load more" clicks) drop their late
   * responses if a newer seq has started.
   */
  const fireSearch = async (q: string, p: number, append: boolean) => {
    if (!hasTauri()) {
      setResults([]);
      setHasMore(false);
      return;
    }
    pendingSearchSeq += 1;
    const mySeq = pendingSearchSeq;
    setLoading(true);
    try {
      const resp = await search(q, p, locale());
      if (mySeq !== pendingSearchSeq) return;
      applyResponse(resp, append);
      if (
        resp.direct === null &&
        resp.results.length > 0 &&
        !append
      ) {
        // Server-side `search` already upserts on success, but trigger a
        // refetch so the recent-searches surface shows the new entry the
        // moment the user clears the input.
        void refetchRecent();
      }
    } catch (e) {
      if (mySeq !== pendingSearchSeq) return;
      setError(e instanceof Error ? e.message : String(e));
      setResults([]);
      setHasMore(false);
    } finally {
      if (mySeq === pendingSearchSeq) setLoading(false);
    }
  };

  // Trigger a fresh search whenever the debounced query (or locale)
  // changes. Empty queries clear the result list and show the recent-
  // searches surface.
  createEffect(
    on([activeQuery, locale], ([q]) => {
      if (q.length === 0) {
        setResults([]);
        setHasMore(false);
        setError(null);
        return;
      }
      void fireSearch(q, 1, false);
    }),
  );

  const loadMore = () => {
    if (loading() || !hasMore()) return;
    const next = page() + 1;
    setPage(next);
    void fireSearch(activeQuery(), next, true);
  };

  /**
   * Activation handler for a result tile. Mirrors `Home`'s pattern:
   * remember the originating focus id, then navigate. PRD §F-010
   * "Back navigation returns focus to the originating tile" is honored
   * because the detail route pops this stack.
   */
  const activateTile = (summary: TitleSummary) => {
    const here = focusedId();
    if (here !== null) pushReturnFocus(here);
    navigate(
      `/title/${encodeURIComponent(summary.id)}?kind=${summary.kind}`,
    );
  };

  /**
   * Re-search a recent-search entry. Loads the query into the input
   * AND advances the debounced `activeQuery` so the existing fetch
   * effect fires without waiting for a 300ms keystroke debounce. The
   * effect (subscribed to `activeQuery`) handles the actual search
   * call so we don't double-fire.
   */
  const activateRecent = (q: string) => {
    if (debounceHandle !== null) {
      clearTimeout(debounceHandle);
      debounceHandle = null;
    }
    setQuery(q);
    setPage(1);
    if (inputEl) inputEl.value = q;
    // Force the effect to re-fire even if `activeQuery` already equals
    // `q` (e.g. user re-activates the most recent entry). Cycling
    // through empty re-triggers the effect's createEffect.
    if (activeQuery() === q) {
      setActiveQuery("");
    }
    setActiveQuery(q);
  };

  const clearRecent = async () => {
    if (!hasTauri()) return;
    try {
      await recentSearchesClear();
      void refetchRecent();
    } catch (e) {
      console.warn("recent_searches_clear failed", e);
    }
  };

  // PRD §F-011: search bar is focused on entry. Autofocus the input
  // when the route mounts AND when the input id becomes the focus
  // target (e.g. via the global `/` shortcut from App.tsx).
  onMount(() => {
    // Defer to next microtask so the input ref is set.
    queueMicrotask(() => {
      inputEl?.focus();
      setFocusedId(SEARCH_INPUT_FOCUS_ID);
    });
  });

  createEffect(() => {
    if (focusedId() === SEARCH_INPUT_FOCUS_ID && inputEl) {
      // Focus stays in the manager; sync the DOM focus so typing lands
      // in the input regardless of which path claimed focus.
      inputEl.focus();
    }
  });

  onCleanup(() => {
    if (debounceHandle !== null) {
      clearTimeout(debounceHandle);
      debounceHandle = null;
    }
    pendingSearchSeq += 1; // invalidate in-flight responses
  });

  const showResults = createMemo(() => activeQuery().length > 0);
  const recentEntries = createMemo<string[]>(() => recent() ?? []);

  return (
    <div
      class="flex h-full w-full flex-col overflow-y-auto"
      data-testid="search-route"
    >
      <header class="sticky top-0 z-10 flex flex-col gap-2 bg-neutral-950/95 px-6 py-6 backdrop-blur">
        <h1
          class="text-3xl font-bold text-neutral-50"
          data-testid="search-title"
        >
          {t("search.title")}
        </h1>
        <Focusable id={SEARCH_INPUT_FOCUS_ID}>
          {({ focused, showRing, ref, onClick }) => (
            <div
              class={`flex items-center rounded-md border bg-neutral-900 px-3 py-2 transition-colors ${
                showRing()
                  ? "border-sky-400 outline outline-2 outline-sky-400"
                  : "border-neutral-700"
              }`}
              ref={ref as (el: HTMLDivElement) => void}
              data-focused={focused() ? "true" : "false"}
            >
              <span
                aria-hidden="true"
                class="mr-2 text-neutral-400"
              >
                ⌕
              </span>
              <input
                ref={(el) => {
                  inputEl = el;
                }}
                type="search"
                inputmode="search"
                autocomplete="off"
                spellcheck={false}
                value={query()}
                placeholder={t("search.placeholder")}
                onInput={(e) => handleInput(e.currentTarget.value)}
                onFocus={onClick}
                class="w-full bg-transparent text-base text-neutral-50 outline-none placeholder:text-neutral-500"
                data-testid={SEARCH_INPUT_TEST_ID}
                aria-label={t("search.title")}
              />
            </div>
          )}
        </Focusable>
        <p class="text-xs text-neutral-500" data-testid="search-hint">
          {t("search.hint")}
        </p>
      </header>

      <Show
        when={showResults()}
        fallback={
          <RecentSearchesPanel
            entries={recentEntries()}
            onActivate={activateRecent}
            onClear={clearRecent}
          />
        }
      >
        <ResultsPanel
          query={activeQuery()}
          results={results()}
          hasMore={hasMore()}
          loading={loading()}
          error={error()}
          onActivate={activateTile}
          onLoadMore={loadMore}
        />
      </Show>
    </div>
  );
};

type RecentPanelProps = {
  entries: string[];
  onActivate: (q: string) => void;
  onClear: () => void;
};

const RecentSearchesPanel: Component<RecentPanelProps> = (props) => {
  return (
    <section class="flex flex-col gap-3 px-6 py-4" data-testid="search-recent">
      <div class="flex items-center justify-between">
        <h2 class="text-lg font-semibold tracking-wide text-neutral-200">
          {t("search.recentTitle")}
        </h2>
        <Show when={props.entries.length > 0}>
          <Focusable
            id="search-recent-clear"
            onActivate={() => props.onClear()}
          >
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                class={`rounded-md px-3 py-1 text-sm text-neutral-300 transition-colors ${
                  showRing()
                    ? "outline outline-2 outline-sky-400"
                    : "hover:bg-neutral-900"
                }`}
                data-testid="search-recent-clear"
              >
                {t("search.recentClear")}
              </button>
            )}
          </Focusable>
        </Show>
      </div>
      <Show
        when={props.entries.length > 0}
        fallback={
          <p
            class="text-sm text-neutral-500"
            data-testid="search-recent-empty"
          >
            —
          </p>
        }
      >
        <ul class="flex flex-col gap-1" data-testid="search-recent-list">
          <For each={props.entries}>
            {(entry) => (
              <li>
                <Focusable
                  id={`search-recent-${entry}`}
                  onActivate={() => props.onActivate(entry)}
                >
                  {({ showRing, ref, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      onClick={onClick}
                      class={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-neutral-200 transition-colors ${
                        showRing()
                          ? "outline outline-2 outline-sky-400"
                          : "hover:bg-neutral-900"
                      }`}
                      data-testid={`search-recent-item-${entry}`}
                    >
                      <span aria-hidden="true" class="text-neutral-500">
                        ⌛
                      </span>
                      <span class="truncate">{entry}</span>
                    </button>
                  )}
                </Focusable>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
};

type ResultsPanelProps = {
  query: string;
  results: TitleSummary[];
  hasMore: boolean;
  loading: boolean;
  error: string | null;
  onActivate: (s: TitleSummary) => void;
  onLoadMore: () => void;
};

const ResultsPanel: Component<ResultsPanelProps> = (props) => {
  return (
    <section
      class="flex flex-col gap-4 px-6 py-4"
      data-testid="search-results"
    >
      <Show when={props.loading && props.results.length === 0}>
        <p class="text-sm text-neutral-400" data-testid="search-loading">
          {t("search.loading")}
        </p>
      </Show>
      <Show when={props.error !== null}>
        <p class="text-sm text-rose-400" data-testid="search-error">
          {props.error}
        </p>
      </Show>
      <Show
        when={!props.loading || props.results.length > 0}
      >
        <Show
          when={props.results.length > 0}
          fallback={
            <Show when={!props.loading}>
              <p
                class="text-sm text-neutral-500"
                data-testid="search-empty"
              >
                {t("search.empty")}
              </p>
            </Show>
          }
        >
          <div
            class="flex flex-wrap gap-3"
            role="list"
            data-testid="search-results-grid"
          >
            <For each={props.results}>
              {(item) => (
                <div role="listitem" class="flex-shrink-0">
                  <Tile
                    focusId={`search-result-${item.kind}-${item.id}`}
                    summary={item}
                    onActivate={() => props.onActivate(item)}
                  />
                </div>
              )}
            </For>
          </div>
          <Show when={props.hasMore}>
            <Focusable
              id="search-load-more"
              onActivate={() => props.onLoadMore()}
            >
              {({ showRing, ref, onClick }) => (
                <button
                  ref={ref as (el: HTMLButtonElement) => void}
                  onClick={onClick}
                  disabled={props.loading}
                  class={`mt-2 self-start rounded-md border border-neutral-700 bg-neutral-900 px-4 py-2 text-sm text-neutral-100 transition-colors ${
                    showRing()
                      ? "outline outline-2 outline-sky-400"
                      : "hover:bg-neutral-800"
                  } ${props.loading ? "opacity-60" : ""}`}
                  data-testid="search-load-more"
                >
                  {props.loading
                    ? t("search.loading")
                    : t("search.loadMore")}
                </button>
              )}
            </Focusable>
          </Show>
        </Show>
      </Show>
    </section>
  );
};
