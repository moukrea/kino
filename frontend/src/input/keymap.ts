// PRD §F-017 action-mapping tables — Android TV (D-pad + gamepad),
// Android mobile (touch primary, gamepad / KBM secondary), and Linux
// (KBM primary). The full per-platform tables live in PRD §F-017; this
// module encodes them as a typed lookup so the rest of the input
// subsystem (`keyboard.ts`, `gamepad.ts`, focus traversal) can dispatch
// generically.
//
// `Action` is the canonical app-level intent; per-platform secondary
// inputs (gamepad on Linux, KBM on Android TV, etc.) all collapse to
// the same Action set so application code only listens to actions, not
// raw device events.

import type { InputProfile } from "./profile";

export type Action =
  | "navigate-up"
  | "navigate-down"
  | "navigate-left"
  | "navigate-right"
  | "activate"
  | "back"
  | "context"
  | "search"
  | "play-pause";

/**
 * Web `KeyboardEvent.code` (or `key` for non-printable specials)
 * mappings, locked to PRD §F-017 KBM columns. We prefer `code` for
 * arrow / letter keys (layout-independent) and `key` for `Enter`,
 * `Escape`, etc. — the keyboard handler tries both lookups in turn.
 */
const KEYBOARD_MAP: Record<string, Action> = {
  ArrowUp: "navigate-up",
  ArrowDown: "navigate-down",
  ArrowLeft: "navigate-left",
  ArrowRight: "navigate-right",
  Enter: "activate",
  Escape: "back",
  F10: "context",
  "/": "search",
  Slash: "search",
  " ": "play-pause",
  Space: "play-pause",
};

/**
 * Web Gamepad API standard button indices — locked subset from
 * https://www.w3.org/TR/gamepad/#dom-gamepad-buttons. PRD §F-017's
 * gamepad columns ("A activates", "B back", "Y context / search",
 * "Start play-pause") collapse to these indices.
 */
export const GAMEPAD_BUTTONS = {
  A: 0,
  B: 1,
  X: 2,
  Y: 3,
  Start: 9,
  DpadUp: 12,
  DpadDown: 13,
  DpadLeft: 14,
  DpadRight: 15,
} as const;

const GAMEPAD_MAP: Record<number, Action> = {
  [GAMEPAD_BUTTONS.A]: "activate",
  [GAMEPAD_BUTTONS.B]: "back",
  [GAMEPAD_BUTTONS.Y]: "context",
  [GAMEPAD_BUTTONS.Start]: "play-pause",
  [GAMEPAD_BUTTONS.DpadUp]: "navigate-up",
  [GAMEPAD_BUTTONS.DpadDown]: "navigate-down",
  [GAMEPAD_BUTTONS.DpadLeft]: "navigate-left",
  [GAMEPAD_BUTTONS.DpadRight]: "navigate-right",
};

/**
 * Special-case: the Y button doubles as "context" and as "focus
 * search from home" (PRD §F-017 Android TV / Linux gamepad columns).
 * The focus-search behavior is a route-level concern handled in the
 * Home screen (F-008); the input layer only emits `context`.
 */

/**
 * Resolve a keyboard event to an Action. Returns `null` if the event
 * doesn't map. The handler is layout-independent: it tries the
 * physical `code` first, then falls back to the printable `key`.
 *
 * Modifier keys (Ctrl, Meta, Alt) are NOT consumed — chord-shortcuts
 * stay available for the surrounding shell.
 */
export function keyboardEventToAction(event: {
  code?: string;
  key?: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}): Action | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.code !== undefined) {
    const action = KEYBOARD_MAP[event.code];
    if (action !== undefined) return action;
  }
  if (event.key !== undefined) {
    const action = KEYBOARD_MAP[event.key];
    if (action !== undefined) return action;
  }
  return null;
}

/**
 * Resolve a gamepad button index to an Action.
 */
export function gamepadButtonToAction(index: number): Action | null {
  return GAMEPAD_MAP[index] ?? null;
}

/**
 * PRD §F-017: gamepad and KBM are SECONDARY on every platform; the
 * input layer listens to all three device classes regardless of the
 * active profile and just emits Actions. The "primary" column in the
 * PRD per-platform tables controls focus VISUALS only — d-pad /
 * gamepad / kbm profiles show focus rings; touch profile hides them
 * until interaction.
 */
export function showsFocusRing(profile: InputProfile): boolean {
  return profile === "dpad" || profile === "gamepad" || profile === "kbm";
}
