// Home / catalog tile. PRD §F-008 specs (locked):
//
//   - Poster aspect 2:3, base 240×360 reference
//   - Focus state: scale 1.08, soft shadow, border glow
//   - Focus transition: 150ms ease-out
//   - Title + year overlaid on focused tile only
//   - Info overlay slides in after 600ms of held focus
//
// The tile registers itself with the focus manager via `<Focusable>`
// (PRD §F-017 / ADR-063). The 600ms info overlay is a per-tile timer
// armed when this tile claims focus and cleared on blur, on activation,
// or on unmount.
//
// Image lazy-loading: tiles set `loading="lazy"` on the `<img>` so the
// browser owns viewport-based deferral. Combined with `Row`'s windowing
// (only nearby tiles in the DOM tree) this satisfies PRD §F-008's
// "rows lazy-load tiles beyond viewport (virtualization)" acceptance.

import {
  createSignal,
  onCleanup,
  Show,
  type Component,
} from "solid-js";

import { Focusable } from "./Focusable";
import type { TitleSummary } from "../lib/tauri";

/**
 * PRD §F-008: info overlay surfaces after 600ms of held focus.
 */
export const INFO_OVERLAY_DELAY_MS = 600;

export type TileProps = {
  /**
   * Stable id for the focus manager. The caller scopes it with a row
   * prefix (e.g. `"row-trending-tt0133093"`) so two rows can render
   * the same title without colliding in the focus registry.
   */
  focusId: string;
  /**
   * Summary the tile displays. `poster` is rendered when present; the
   * fallback below is a placeholder block with the title text.
   */
  summary: TitleSummary;
  /**
   * Optional badge text rendered above the focused-tile caption.
   * Used by the F-012 Continue Watching row to surface "Resume Sxx
   * Eyy" / "Up next: Sxx Eyy" labels per PRD §F-012 series rules.
   */
  badge?: string | null;
  /**
   * Invoked when the user activates the tile (Enter / A / tap / click).
   */
  onActivate?: () => void;
  /**
   * Optional PRD §F-012 manual-remove handler. When set, the tile
   * accepts Y (gamepad) / Menu (D-pad) / right-click / long-press as
   * context actions. The Home CW row wires this to wipe the title's
   * CW rows; other rows leave it unset.
   */
  onContext?: () => void;
};

export const Tile: Component<TileProps> = (props) => {
  const [overlayVisible, setOverlayVisible] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  const armOverlay = () => {
    cancelOverlay();
    timer = setTimeout(() => {
      setOverlayVisible(true);
      timer = null;
    }, INFO_OVERLAY_DELAY_MS);
  };

  const cancelOverlay = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    setOverlayVisible(false);
  };

  onCleanup(cancelOverlay);

  const yearLabel = () => {
    const y = props.summary.year;
    return y === null ? "" : String(y);
  };

  return (
    <Focusable
      id={props.focusId}
      onActivate={props.onActivate}
      onContext={props.onContext}
      onFocus={armOverlay}
      onBlur={cancelOverlay}
    >
      {({
        focused,
        showRing,
        ref,
        onClick,
        onContextMenu,
        onTouchStart,
        onTouchEnd,
        onTouchMove,
        onTouchCancel,
      }) => (
        <button
          ref={ref as (el: HTMLButtonElement) => void}
          onClick={() => {
            cancelOverlay();
            onClick();
          }}
          onContextMenu={onContextMenu}
          onTouchStart={onTouchStart}
          onTouchEnd={onTouchEnd}
          onTouchMove={onTouchMove}
          onTouchCancel={onTouchCancel}
          data-testid={`tile-${props.focusId}`}
          data-focused={focused() ? "true" : "false"}
          data-kind={props.summary.kind}
          aria-label={`${props.summary.title}${
            yearLabel() ? ` (${yearLabel()})` : ""
          }`}
          class={`relative flex w-[clamp(140px,18vw,240px)] flex-shrink-0 flex-col overflow-hidden rounded-md bg-neutral-900 text-left transition-transform duration-150 ease-out ${
            showRing()
              ? "z-10 scale-[1.08] shadow-[0_8px_30px_rgba(0,0,0,0.55)] outline outline-2 outline-sky-400"
              : ""
          }`}
        >
          <div class="relative aspect-[2/3] w-full bg-neutral-800">
            <Show
              when={props.summary.poster}
              fallback={
                <div
                  class="flex h-full w-full items-center justify-center p-3 text-center text-sm text-neutral-400"
                  data-testid="tile-poster-placeholder"
                >
                  {props.summary.title}
                </div>
              }
            >
              {(posterUrl) => (
                <img
                  src={posterUrl()}
                  alt=""
                  loading="lazy"
                  decoding="async"
                  class="h-full w-full object-cover"
                />
              )}
            </Show>
            <Show when={focused()}>
              <div
                class="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/85 via-black/50 to-transparent px-2 py-2 text-sm"
                data-testid="tile-caption"
              >
                <Show when={props.badge}>
                  {(badge) => (
                    <div
                      class="mb-1 inline-block rounded bg-sky-500/90 px-1.5 py-0.5 text-xs font-semibold text-neutral-50"
                      data-testid="tile-badge"
                    >
                      {badge()}
                    </div>
                  )}
                </Show>
                <div class="truncate font-medium text-neutral-50">
                  {props.summary.title}
                </div>
                <Show when={yearLabel()}>
                  <div class="text-xs text-neutral-300">{yearLabel()}</div>
                </Show>
              </div>
            </Show>
          </div>
          <Show when={overlayVisible() && focused()}>
            <div
              class="absolute inset-x-0 bottom-0 translate-y-0 rounded-b-md border-t border-neutral-700 bg-neutral-950/95 p-3 text-xs text-neutral-100 shadow-lg transition-transform duration-150 ease-out"
              data-testid="tile-info-overlay"
            >
              <div class="mb-1 font-semibold">
                {props.summary.title}
                <Show when={yearLabel()}>
                  <span class="ml-1 font-normal text-neutral-400">
                    ({yearLabel()})
                  </span>
                </Show>
              </div>
              <Show when={props.summary.rating !== null}>
                <div class="text-neutral-300">
                  ★ {props.summary.rating?.toFixed(1)}
                </div>
              </Show>
            </div>
          </Show>
        </button>
      )}
    </Focusable>
  );
};
