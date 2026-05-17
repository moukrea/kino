// Focus manager for the kino UI. PRD §F-008 / §F-017 require D-pad
// traversal of all rows and tiles; this module owns the registry of
// focusable surfaces and the directional-navigation algorithm that
// translates a `navigate-{up,down,left,right}` Action into a focus
// move.
//
// Design notes:
//
// - Focusable elements register themselves via `registerFocusable`,
//   typically through the `<Focusable>` component. Registration
//   captures the DOM node plus an opaque id; the rect is read
//   on-demand via `getBoundingClientRect()` so the registry survives
//   layout reflows without re-registering.
//
// - Directional navigation uses a geometric scoring algorithm
//   (PRD §F-008 talks about "rows" but does NOT prescribe an
//   algorithm; the geometric approach matches the WICG Spatial
//   Navigation note and feels natural on a 10-foot UI). For a
//   `navigate-right` move from element A, we score every other
//   focusable B as `dx + alpha * dy` and pick the smallest positive
//   score, where alpha penalizes vertical drift (default 4 — empirical
//   sweet spot from Stremio / Plex 10-foot UIs).
//
// - Initial focus is set by `setInitialFocus(id)` from page code;
//   without it the manager picks the first registered focusable as a
//   reasonable default.
//
// - The manager is a singleton (one focus tree per app); SolidJS's
//   reactive system means consumers can subscribe via `focusedId()`
//   to highlight themselves.

import { createSignal } from "solid-js";

import type { Action } from "./keymap";

export type FocusableEntry = {
  id: string;
  element: HTMLElement;
  /** Optional callback fired when this focusable is activated. */
  onActivate?: () => void;
  /** Optional callback fired when this focusable receives focus. */
  onFocus?: () => void;
  /** Optional callback fired when this focusable loses focus. */
  onBlur?: () => void;
};

const registry = new Map<string, FocusableEntry>();
const [focusedId, setFocusedIdInternal] = createSignal<string | null>(null);
export { focusedId };

/**
 * Register a focusable element with the manager. Returns an
 * unregister callback the caller should invoke on cleanup
 * (`onCleanup` in SolidJS, useEffect return in React).
 */
export function registerFocusable(entry: FocusableEntry): () => void {
  registry.set(entry.id, entry);
  // If this is the first focusable to register, focus it by default.
  if (focusedId() === null) {
    setFocusedId(entry.id);
  }
  return () => {
    unregisterFocusable(entry.id);
  };
}

export function unregisterFocusable(id: string): void {
  registry.delete(id);
  if (focusedId() === id) {
    // Fall through to the next available focusable.
    const next = registry.keys().next();
    setFocusedId(next.done ? null : next.value);
  }
}

/**
 * Force focus to a specific id. Used by route changes and the
 * "back navigation returns focus to the originating tile" path
 * (PRD §F-010 code acceptance).
 */
export function setFocusedId(id: string | null): void {
  const previous = focusedId();
  if (previous === id) return;
  if (previous !== null) {
    registry.get(previous)?.onBlur?.();
  }
  setFocusedIdInternal(id);
  if (id !== null) {
    registry.get(id)?.onFocus?.();
  }
}

/**
 * Initial-focus helper used by routes that want a specific surface
 * to claim focus on mount. No-op if the id isn't registered yet —
 * the caller is responsible for ordering.
 */
export function setInitialFocus(id: string): boolean {
  if (!registry.has(id)) return false;
  setFocusedId(id);
  return true;
}

/**
 * Fire the activation callback on the currently focused element.
 */
export function activateFocused(): boolean {
  const id = focusedId();
  if (id === null) return false;
  const entry = registry.get(id);
  if (!entry?.onActivate) return false;
  entry.onActivate();
  return true;
}

/**
 * Read-only accessor used by tests.
 */
export function getRegisteredIds(): readonly string[] {
  return Array.from(registry.keys());
}

/**
 * Drop the registry. Test-only; never call from production code.
 */
export function _resetForTests(): void {
  registry.clear();
  setFocusedIdInternal(null);
  returnFocusStack.length = 0;
}

// PRD §F-010 "Back navigation returns focus to the originating tile":
// before a tile activation navigates to a child route, the focus
// manager remembers which id had focus; the child route's back-path
// pops the saved id and re-focuses it on its way out. A stack handles
// the (eventual) Detail → Detail jump case (e.g. clicking a related
// title); v1 only navigates one level deep but the stack adds no
// runtime cost.
const returnFocusStack: string[] = [];

/**
 * Remember `id` so a future `popReturnFocus()` call (typically in a
 * child route's back handler) can restore focus to it. Idempotent if
 * called with the same id twice in a row (de-dupes consecutive
 * duplicates).
 */
export function pushReturnFocus(id: string): void {
  if (returnFocusStack[returnFocusStack.length - 1] === id) return;
  returnFocusStack.push(id);
}

/**
 * Pop the most recent saved return-focus id, or `null` if the stack is
 * empty. Caller is responsible for setting the focus.
 */
export function popReturnFocus(): string | null {
  return returnFocusStack.pop() ?? null;
}

/**
 * Test-only accessor for the return-focus stack.
 */
export function _returnFocusStackForTests(): readonly string[] {
  return [...returnFocusStack];
}

type Rect = { left: number; top: number; right: number; bottom: number; cx: number; cy: number };

function rectOf(el: HTMLElement): Rect {
  const r = el.getBoundingClientRect();
  return {
    left: r.left,
    top: r.top,
    right: r.right,
    bottom: r.bottom,
    cx: r.left + r.width / 2,
    cy: r.top + r.height / 2,
  };
}

/**
 * Geometric scoring for directional moves. Returns `null` when the
 * candidate is on the wrong side of the origin (e.g. when scoring a
 * `navigate-right` move and the candidate is to the LEFT of origin).
 *
 * Lower scores win. The cross-axis penalty `alpha = 4` penalizes
 * vertical drift heavily so horizontal moves prefer tiles within the
 * same row over tiles on a different row that happen to be closer in
 * absolute distance.
 */
const ALPHA = 4;

function score(direction: NavDirection, from: Rect, to: Rect): number | null {
  switch (direction) {
    case "navigate-right": {
      const dx = to.left - from.right;
      if (dx < 0) return null;
      const dy = Math.abs(to.cy - from.cy);
      return dx + ALPHA * dy;
    }
    case "navigate-left": {
      const dx = from.left - to.right;
      if (dx < 0) return null;
      const dy = Math.abs(to.cy - from.cy);
      return dx + ALPHA * dy;
    }
    case "navigate-down": {
      const dy = to.top - from.bottom;
      if (dy < 0) return null;
      const dx = Math.abs(to.cx - from.cx);
      return dy + ALPHA * dx;
    }
    case "navigate-up": {
      const dy = from.top - to.bottom;
      if (dy < 0) return null;
      const dx = Math.abs(to.cx - from.cx);
      return dy + ALPHA * dx;
    }
  }
}

export type NavDirection =
  | "navigate-up"
  | "navigate-down"
  | "navigate-left"
  | "navigate-right";

export function isNavDirection(action: Action): action is NavDirection {
  return (
    action === "navigate-up" ||
    action === "navigate-down" ||
    action === "navigate-left" ||
    action === "navigate-right"
  );
}

/**
 * Move focus in the requested direction. Returns true if focus moved,
 * false if no eligible target exists (e.g. user pressing Right on
 * the rightmost tile of a row with no further rows).
 *
 * Exception: if no focusable currently has focus, this picks the
 * first registered focusable as a recovery default.
 */
export function moveFocus(direction: NavDirection): boolean {
  const currentId = focusedId();
  if (currentId === null) {
    const first = registry.keys().next();
    if (first.done) return false;
    setFocusedId(first.value);
    return true;
  }
  const from = registry.get(currentId);
  if (!from) return false;
  const fromRect = rectOf(from.element);

  let bestId: string | null = null;
  let bestScore = Number.POSITIVE_INFINITY;
  for (const candidate of registry.values()) {
    if (candidate.id === currentId) continue;
    const candidateRect = rectOf(candidate.element);
    const s = score(direction, fromRect, candidateRect);
    if (s === null) continue;
    if (s < bestScore) {
      bestScore = s;
      bestId = candidate.id;
    }
  }
  if (bestId === null) return false;
  setFocusedId(bestId);
  return true;
}
