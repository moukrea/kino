// Gamepad polling loop. PRD §F-017 acceptance:
//   "Plugging a gamepad mid-session causes focus visuals to adapt
//    within 2s"
//
// The Web Gamepad API is poll-driven: there is no event for a button
// down beyond `gamepadconnected` / `gamepaddisconnected`. We rAF-poll
// at the display refresh rate (≤16ms on 60Hz, well under the 2s
// adaptation budget) and emit Actions on rising edges.
//
// Connect / disconnect events also drive the capability signal so
// `profile.resolveProfile` can flip to / from `gamepad` automatically
// when the override is `"auto"`.

import { activateFocused, isNavDirection, moveFocus } from "./focus";
import { gamepadButtonToAction } from "./keymap";
import { emitAction, type InputSource } from "./keyboard";
import { reportGamepadPresent } from "./profile";

const previousPressed = new Map<string, Set<number>>();
let rafHandle: number | null = null;
let cleanupConnectionListeners: (() => void) | null = null;

const GAMEPAD_SOURCE: InputSource = "gamepad";

/**
 * Process one poll cycle. Exported for tests so they can drive a
 * fake `navigator.getGamepads` shape without a real rAF loop.
 *
 * Returns the list of Actions emitted this cycle so the test
 * harness can assert on them.
 */
export function pollGamepadsOnce(
  fakeGamepads?: (Gamepad | null)[],
): import("./keymap").Action[] {
  const pads = fakeGamepads
    ? fakeGamepads
    : typeof navigator !== "undefined" &&
        typeof navigator.getGamepads === "function"
      ? Array.from(navigator.getGamepads())
      : [];
  const emitted: import("./keymap").Action[] = [];

  for (const pad of pads) {
    if (!pad) continue;
    const padKey = `${pad.index}:${pad.id}`;
    const prev = previousPressed.get(padKey) ?? new Set<number>();
    const next = new Set<number>();
    pad.buttons.forEach((button, idx) => {
      if (button.pressed) next.add(idx);
    });
    // Rising edge: in next, not in prev.
    for (const idx of next) {
      if (prev.has(idx)) continue;
      const action = gamepadButtonToAction(idx);
      if (action === null) continue;
      if (isNavDirection(action)) {
        moveFocus(action);
      } else if (action === "activate") {
        activateFocused();
      }
      emitAction(action, GAMEPAD_SOURCE);
      emitted.push(action);
    }
    previousPressed.set(padKey, next);
  }
  return emitted;
}

/**
 * Start the rAF polling loop. Idempotent; calling twice is a no-op.
 * The loop self-cancels when `stopGamepadPolling` is called or the
 * window is unloaded.
 */
export function startGamepadPolling(target: Window = window): () => void {
  if (rafHandle !== null) return () => stopGamepadPolling(target);

  const onConnected = (event: GamepadEvent) => {
    reportGamepadPresent(true);
    // Re-seed previous-pressed state so the first cycle doesn't
    // treat any held button as a rising edge.
    previousPressed.set(`${event.gamepad.index}:${event.gamepad.id}`, new Set());
  };
  const onDisconnected = (event: GamepadEvent) => {
    previousPressed.delete(`${event.gamepad.index}:${event.gamepad.id}`);
    // Recompute capability based on remaining connected pads.
    const remaining =
      typeof navigator !== "undefined" &&
      typeof navigator.getGamepads === "function"
        ? Array.from(navigator.getGamepads()).some((g) => g !== null)
        : false;
    reportGamepadPresent(remaining);
  };
  target.addEventListener("gamepadconnected", onConnected);
  target.addEventListener("gamepaddisconnected", onDisconnected);
  cleanupConnectionListeners = () => {
    target.removeEventListener("gamepadconnected", onConnected);
    target.removeEventListener("gamepaddisconnected", onDisconnected);
  };

  const loop = () => {
    pollGamepadsOnce();
    rafHandle = target.requestAnimationFrame(loop);
  };
  rafHandle = target.requestAnimationFrame(loop);

  return () => stopGamepadPolling(target);
}

export function stopGamepadPolling(target: Window = window): void {
  if (rafHandle !== null) {
    target.cancelAnimationFrame(rafHandle);
    rafHandle = null;
  }
  cleanupConnectionListeners?.();
  cleanupConnectionListeners = null;
}

/**
 * Drop the rising-edge state. Test-only.
 */
export function _resetForTests(): void {
  previousPressed.clear();
}
