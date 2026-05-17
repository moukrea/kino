// Barrel for the F-017 input subsystem. Two consumer-facing entry
// points:
//
// - `installInputSubsystem(target?)` — installs the keyboard, gamepad
//   polling, and touch listeners on a target window. Returns an
//   `uninstall()` for cleanup (the SolidJS root never tears down in
//   production but tests and HMR need it).
//
// - `onAction(listener)` / `focusedId` / `setInitialFocus` /
//   `moveFocus` / etc. — the per-feature API surface.

import {
  installKeyboardListener,
  uninstallKeyboardListener,
} from "./keyboard";
import {
  installTouchListener,
  uninstallTouchListener,
} from "./touch";
import {
  startGamepadPolling,
  stopGamepadPolling,
} from "./gamepad";

export type { Action } from "./keymap";
export type {
  Capabilities,
  InputProfile,
  InputProfileOverride,
  Platform,
} from "./profile";
export type { InputSource } from "./keyboard";
export type { NavDirection, FocusableEntry } from "./focus";

export {
  focusedId,
  isNavDirection,
  moveFocus,
  registerFocusable,
  setFocusedId,
  setInitialFocus,
  activateFocused,
  unregisterFocusable,
  getRegisteredIds,
  pushReturnFocus,
  popReturnFocus,
} from "./focus";

export {
  detectCapabilities,
  detectPlatform,
  defaultProfileForPlatform,
  capabilities,
  override,
  platform,
  profile,
  reportGamepadPresent,
  reportTouchPresent,
  reportKeyboardPresent,
  resolveProfile,
  setOverride,
  setPlatform,
} from "./profile";

export { emitAction, handleKeyboardEvent, onAction } from "./keyboard";
export { showsFocusRing } from "./keymap";
export { handleTouchEvent } from "./touch";
export { pollGamepadsOnce } from "./gamepad";

/**
 * Install every input listener for the live app. Idempotent; calling
 * twice is a no-op. Returns an uninstall callback for tests / HMR.
 */
export function installInputSubsystem(target: Window = window): () => void {
  const uninstallKb = installKeyboardListener(target);
  const uninstallTouch = installTouchListener(target);
  const uninstallGp = startGamepadPolling(target);
  return () => {
    uninstallKb();
    uninstallTouch();
    uninstallGp();
  };
}

export function uninstallInputSubsystem(target: Window = window): void {
  uninstallKeyboardListener(target);
  uninstallTouchListener(target);
  stopGamepadPolling(target);
}
