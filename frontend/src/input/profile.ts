// PRD §F-017 "Input handling":
//   "Runtime input profile detection. App auto-selects on launch and adapts
//    when devices appear/disappear. User can force a profile in settings."
//
// Four profiles are recognized — kept in lockstep with the per-platform
// action-mapping tables in `keymap.ts` and the persisted Display setting
// "Input profile override" (auto / touch / dpad / kbm) from PRD §F-016 §7.
//
// "Gamepad" is intentionally kept distinct from "dpad". A user holding a
// physical gamepad on Linux still gets KBM-style focus visuals by default
// (PRD's per-platform tables list gamepad as a SECONDARY input on every
// platform); we only switch the profile when the active platform's
// PRIMARY input is gamepad-shaped (Android TV).

import { createMemo, createSignal } from "solid-js";

export type InputProfile = "touch" | "dpad" | "kbm" | "gamepad";

export type InputProfileOverride = InputProfile | "auto";

export type Platform = "android-tv" | "android-mobile" | "linux";

/**
 * The PRD-locked default profile per platform — the "primary" column from
 * the §F-017 per-platform action-mapping tables.
 */
export function defaultProfileForPlatform(platform: Platform): InputProfile {
  switch (platform) {
    case "android-tv":
      return "dpad";
    case "android-mobile":
      return "touch";
    case "linux":
      return "kbm";
  }
}

export type Capabilities = {
  hasTouch: boolean;
  hasGamepad: boolean;
  hasKeyboard: boolean;
};

/**
 * Pure function — given a platform, its baseline default, the current
 * device capability snapshot, and any user override, returns the profile
 * the app should be in.
 *
 * Rules (PRD §F-017):
 * - User override always wins unless it's `"auto"`.
 * - Android TV stays on `dpad` even if a keyboard is plugged in (a
 *   pluggable USB keyboard on Shield is supplementary, not primary).
 * - Android mobile uses `touch` unless a gamepad is connected, in which
 *   case the user is probably docked / on Android TV-like hardware and
 *   should get `gamepad`.
 * - Linux uses `kbm` unless ONLY a gamepad capability is present (e.g.
 *   the user disconnected the keyboard) — but a desktop without keyboard
 *   is exotic enough that we still default to KBM. A live runtime
 *   "gamepad just connected" signal upgrades to `gamepad` via the
 *   capability snapshot.
 */
export function resolveProfile(
  platform: Platform,
  caps: Capabilities,
  override: InputProfileOverride,
): InputProfile {
  if (override !== "auto") return override;
  switch (platform) {
    case "android-tv":
      return "dpad";
    case "android-mobile":
      if (caps.hasGamepad) return "gamepad";
      return "touch";
    case "linux":
      if (caps.hasGamepad && !caps.hasKeyboard) return "gamepad";
      return "kbm";
  }
}

/**
 * Best-effort browser-side platform detection. Tauri exposes
 * `@tauri-apps/api/os` for an authoritative OS string, but the frontend
 * loads identically across web previews and native windows; this
 * fallback uses UA hints so a `vite preview` smoke test still resolves
 * a sensible profile.
 *
 * Android TV vs Android mobile detection is best-effort: the
 * `androidtv` UA token is the conventional signal. Real-world TV
 * detection on the Shield Pro 2019 is verified in §6B-3.
 */
export function detectPlatform(): Platform {
  if (typeof navigator === "undefined") return "linux";
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("android")) {
    if (ua.includes("tv") || ua.includes("googletv") || ua.includes("smart-tv")) {
      return "android-tv";
    }
    return "android-mobile";
  }
  return "linux";
}

/**
 * Best-effort initial capability sniff. `navigator.maxTouchPoints` is
 * the modern source of truth for touch capability; the legacy
 * `ontouchstart` window check is a fallback for environments that
 * didn't update the new API (e.g. very old Android WebViews).
 */
export function detectCapabilities(): Capabilities {
  if (typeof navigator === "undefined" || typeof window === "undefined") {
    return { hasTouch: false, hasGamepad: false, hasKeyboard: true };
  }
  const hasTouch =
    (typeof navigator.maxTouchPoints === "number" &&
      navigator.maxTouchPoints > 0) ||
    "ontouchstart" in window;
  const hasGamepad =
    typeof navigator.getGamepads === "function" &&
    Array.from(navigator.getGamepads()).some((g) => g !== null);
  // We assume the presence of a keyboard on every platform except
  // pure-touch contexts; Android TV explicitly retains `hasKeyboard`
  // because the Shield remote exposes keyboard-shaped events.
  const hasKeyboard = true;
  return { hasTouch, hasGamepad, hasKeyboard };
}

const [override, setOverrideInternal] = createSignal<InputProfileOverride>("auto");
const [platform, setPlatform] = createSignal<Platform>(detectPlatform());
const [capabilities, setCapabilities] = createSignal<Capabilities>(
  detectCapabilities(),
);

const profile = createMemo<InputProfile>(() =>
  resolveProfile(platform(), capabilities(), override()),
);

export { capabilities, override, platform, profile };
export { setPlatform };

/**
 * Override setter exposed to the Settings screen (F-016) and unit tests.
 * Persistence to `kv_*` is the Settings screen's responsibility — this
 * module owns the in-memory state only.
 */
export function setOverride(next: InputProfileOverride): void {
  setOverrideInternal(next);
}

/**
 * Capability mutators called from the runtime device watchers
 * (`keyboard.ts`, `gamepad.ts`, `touch.ts`). Each returns the merged
 * capability object so tests can chain assertions.
 */
export function reportGamepadPresent(present: boolean): Capabilities {
  const next = { ...capabilities(), hasGamepad: present };
  setCapabilities(next);
  return next;
}

export function reportTouchPresent(present: boolean): Capabilities {
  const next = { ...capabilities(), hasTouch: present };
  setCapabilities(next);
  return next;
}

export function reportKeyboardPresent(present: boolean): Capabilities {
  const next = { ...capabilities(), hasKeyboard: present };
  setCapabilities(next);
  return next;
}

/**
 * Test-only reset. Returns the global state to defaults so vitest's
 * shared-module model doesn't leak between cases.
 */
export function _resetForTests(): void {
  setOverrideInternal("auto");
  setPlatform(detectPlatform());
  setCapabilities(detectCapabilities());
}
