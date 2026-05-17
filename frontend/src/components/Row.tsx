// Home / catalog row. PRD §F-008 acceptance:
//
//   - "Rows lazy-load tiles beyond viewport (virtualization)"
//   - Row label (Continue Watching, Trending Now, etc.) above the
//     horizontal track
//   - Tiles registered as focusables so D-pad / arrow / gamepad
//     traversal sees them
//
// Virtualization strategy:
//
//   The row keeps a window of `INITIAL_WINDOW` tiles in the DOM
//   initially. As the user scrolls focus right (via the focus manager's
//   directional nav), the row grows the window by `WINDOW_STEP` tiles
//   each time focus moves into the last few rendered tiles. Tiles past
//   the window are NOT in the DOM at all — neither their `<img>` nor
//   their focusable registration — so a 200-item addon catalog only
//   pays render cost for the part the user has actually scrolled
//   through.
//
//   Browser-level `loading="lazy"` on tile `<img>` handles the
//   in-window viewport-defer case. Combined the two give us the
//   per-row "virtualization" the PRD asks for without pulling in a
//   third-party virtual-list library.
//
//   The row also auto-scrolls the focused tile into view via the
//   `focused` accessor — when a tile claims focus, its host element's
//   `scrollIntoView({ inline: "center", block: "nearest" })` keeps it
//   visible inside the horizontally-scrolling track.

import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  Show,
  type Component,
  type JSX,
} from "solid-js";

import { focusedId } from "../input/focus";
import { Tile } from "./Tile";
import type { TitleSummary } from "../lib/tauri";

/**
 * Initial number of tiles rendered into the DOM. Picked to comfortably
 * fill the visible part of the track on a 1080p screen (240px tiles +
 * gap ≈ 8 tiles wide) plus a little headroom so the user can press
 * Right a few times before the window grows.
 */
export const INITIAL_WINDOW = 12;

/**
 * How many additional tiles to materialize each time focus reaches the
 * tail of the current window. A `WINDOW_STEP` of 6 means the user gets
 * 6 more tiles per "near-tail" focus event — enough to keep up with
 * fast horizontal scrolling without re-rendering on every step.
 */
export const WINDOW_STEP = 6;

/**
 * Distance from the tail at which the window grows. With `TAIL_TRIGGER
 * = 3` and `INITIAL_WINDOW = 12`, the window expands as soon as focus
 * reaches index 9; that gives the next batch of tiles time to render
 * before the user reaches the new tail.
 */
export const TAIL_TRIGGER = 3;

export type RowProps = {
  /**
   * Display label rendered above the track (already translated).
   */
  label: string;
  /**
   * Stable id prefix for focusable tile ids in this row. Tiles end up
   * as `${focusIdPrefix}-${summary.id}`.
   */
  focusIdPrefix: string;
  /**
   * Tile data, full set. The row renders only `INITIAL_WINDOW`
   * elements initially and grows as the user scrolls.
   */
  items: TitleSummary[];
  /**
   * Activation handler forwarded to each tile.
   */
  onActivate?: (summary: TitleSummary) => void;
  /**
   * Optional rendering override for empty rows. Default is a single-
   * line muted placeholder; callers like the home screen can pass
   * `null` to render nothing (useful for the CW row whose empty state
   * must hide the row entirely per PRD §F-008 acceptance).
   */
  emptyFallback?: JSX.Element | null;
  /**
   * Optional test hook so consumers can wait on row-ready in vitest.
   */
  testId?: string;
};

export const Row: Component<RowProps> = (props) => {
  // The window expands monotonically as focus drifts toward the tail;
  // it never shrinks, so user navigation stays smooth across the row.
  const [windowSize, setWindowSize] = createSignal(INITIAL_WINDOW);

  const visibleItems = createMemo(() =>
    props.items.slice(0, Math.min(props.items.length, windowSize())),
  );

  // Grow the window when focus is near the tail of the current window.
  // Subscribe to `focusedId` so each focus change re-checks.
  const grow = () => {
    const fid = focusedId();
    if (fid === null) return;
    const items = visibleItems();
    if (items.length >= props.items.length) return;
    const tailIndex = items.length - 1;
    const tailItem = items[tailIndex];
    if (!tailItem) return;
    // Match the prefix to recognize "this row's" focus events.
    const prefix = `${props.focusIdPrefix}-`;
    if (!fid.startsWith(prefix)) return;
    const focusedSuffix = fid.slice(prefix.length);
    // Find the rendered index of the focused tile.
    const idx = items.findIndex((s) => s.id === focusedSuffix);
    if (idx < 0) return;
    if (idx >= items.length - TAIL_TRIGGER) {
      setWindowSize((current) =>
        Math.min(current + WINDOW_STEP, props.items.length),
      );
    }
  };

  // Subscribe to focus changes: every change re-evaluates the
  // window-growth predicate. `createEffect` is the Solid primitive for
  // side-effect-only reactive blocks.
  createEffect(() => {
    // Touching `focusedId()` subscribes this effect to focus changes.
    void focusedId();
    grow();
  });

  // Auto-scroll the focused tile into view inside the horizontally-
  // scrolling track. Decoupled from the growth effect so a window grow
  // that doesn't move focus doesn't trigger a scroll.
  let trackEl: HTMLDivElement | null = null;
  createEffect(() => {
    const fid = focusedId();
    if (fid === null || trackEl === null) return;
    const prefix = `${props.focusIdPrefix}-`;
    if (!fid.startsWith(prefix)) return;
    // Defer to after Solid commits the DOM mutation. jsdom doesn't
    // implement `scrollIntoView`, so guard the call so vitest doesn't
    // blow up on a missing browser API.
    queueMicrotask(() => {
      const el = trackEl?.querySelector<HTMLElement>(
        `[data-testid="tile-${cssEscape(fid)}"]`,
      );
      if (el && typeof el.scrollIntoView === "function") {
        el.scrollIntoView({
          inline: "center",
          block: "nearest",
          behavior: "auto",
        });
      }
    });
  });

  onCleanup(() => {
    trackEl = null;
  });

  return (
    <section
      class="flex flex-col gap-2"
      data-testid={props.testId ?? `row-${props.focusIdPrefix}`}
    >
      <h2 class="px-6 text-lg font-semibold tracking-wide text-neutral-200">
        {props.label}
      </h2>
      <Show
        when={props.items.length > 0}
        fallback={
          props.emptyFallback === undefined ? (
            <div
              class="px-6 text-sm text-neutral-500"
              data-testid="row-empty-fallback"
            >
              —
            </div>
          ) : (
            props.emptyFallback
          )
        }
      >
        <div
          ref={(el) => {
            trackEl = el;
          }}
          class="flex gap-3 overflow-x-auto scroll-smooth px-6 pb-3"
          data-testid="row-track"
          role="list"
        >
          <For each={visibleItems()}>
            {(item) => (
              <div role="listitem" class="flex-shrink-0">
                <Tile
                  focusId={`${props.focusIdPrefix}-${item.id}`}
                  summary={item}
                  onActivate={() => props.onActivate?.(item)}
                />
              </div>
            )}
          </For>
        </div>
      </Show>
    </section>
  );
};

/**
 * Conservative `CSS.escape` shim — focus ids are alphanumerics, dashes
 * and dots in practice (id shapes like `tmdb:603` / `imdb:tt0133093`),
 * so we only need to escape the colon. Avoids depending on jsdom's
 * `window.CSS` which is patchy across environments.
 */
function cssEscape(id: string): string {
  return id.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}
