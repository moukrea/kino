// PRD §F-006 "Show unavailable titles" toggle, hoisted to a module-level
// signal so the Home / sub-home / catalog routes react live when the user
// flips it in Settings without needing a route remount.
//
// The pattern mirrors `setInputOverride` in `input/profile.ts`: App.tsx
// seeds the signal at boot from `settingsGetAll().display.show_unavailable`
// (PRD §F-016 "All settings persist across restarts"), Settings.tsx writes
// to it on every persist via `setShowUnavailable(...)`, and consumers
// subscribe via the `showUnavailable()` accessor. The backend's persisted
// value is the source of truth; the signal is a cached read-through.
//
// Default `false` is locked by PRD §F-006: "Setting 'Show unavailable
// titles' (default OFF) toggles unavailable tiles to render with a 'no
// source' badge". An "unknown / not yet loaded" state collapses to the
// PRD default so the first paint doesn't flash unavailable tiles before
// the boot-time settings load resolves.

import { createSignal } from "solid-js";

const [showUnavailableSignal, setShowUnavailableSignal] = createSignal(false);

/**
 * Reactive accessor for the PRD §F-006 "show unavailable" toggle. Use
 * this from row / tile consumer code so flipping the Settings toggle
 * re-renders without a route remount.
 */
export const showUnavailable = showUnavailableSignal;

/**
 * Write the PRD §F-006 "show unavailable" flag. Called by App.tsx on
 * boot (from `settingsGetAll`) and by Settings.tsx on every persist of
 * `display.show_unavailable` so the live toggle is reactive across
 * route boundaries.
 */
export function setShowUnavailable(value: boolean): void {
  setShowUnavailableSignal(value);
}

/**
 * Test-only hook: restore the signal to its PRD-locked default so a
 * preceding test's toggle doesn't bleed into the next one. Underscore
 * prefix marks it as not part of the public surface.
 */
export function _resetForTests(): void {
  setShowUnavailableSignal(false);
}
