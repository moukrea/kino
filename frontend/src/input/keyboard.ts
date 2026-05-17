// Keyboard event handler. Subscribes to window `keydown`, decodes the
// event to an Action via `keymap.keyboardEventToAction`, and routes the
// Action to the focus manager (for nav / activate) or emits it on the
// shared action bus (for back / context / search / play-pause, which
// route-level code listens to).
//
// The handler is platform-agnostic: PRD §F-017 says every profile
// listens to keyboards (the Shield remote exposes keyboard events,
// touch users may pair a Bluetooth keyboard, etc.). The decision of
// what to DO with an Action — focus visuals, prompt show/hide,
// route navigation — lives in the consumer code.

import {
  activateFocused,
  isNavDirection,
  moveFocus,
} from "./focus";
import { keyboardEventToAction } from "./keymap";
import { reportKeyboardPresent } from "./profile";

type ActionListener = (action: import("./keymap").Action, source: InputSource) => void;
export type InputSource = "keyboard" | "gamepad" | "touch";

const actionListeners = new Set<ActionListener>();

/**
 * Subscribe to non-navigation actions (`back`, `context`, `search`,
 * `play-pause`, plus `activate` so route-level code can react to
 * Enter / A / etc.). Returns an unsubscribe callback.
 */
export function onAction(listener: ActionListener): () => void {
  actionListeners.add(listener);
  return () => actionListeners.delete(listener);
}

/**
 * Emit an action to every subscriber. Exported so the gamepad and
 * touch handlers can share the same bus.
 */
export function emitAction(
  action: import("./keymap").Action,
  source: InputSource,
): void {
  for (const listener of actionListeners) {
    listener(action, source);
  }
}

/**
 * Drop all listeners. Test-only.
 */
export function _resetForTests(): void {
  actionListeners.clear();
}

/**
 * Process a synthetic keyboard event. Exported so tests can drive
 * the handler without dispatching real `KeyboardEvent`s. Returns
 * the resolved Action (or `null` if the event didn't map), which is
 * also the value the production `keydown` listener returns to the
 * test harness.
 */
export function handleKeyboardEvent(event: {
  code?: string;
  key?: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  preventDefault?: () => void;
}): import("./keymap").Action | null {
  const action = keyboardEventToAction(event);
  if (action === null) return null;
  event.preventDefault?.();
  reportKeyboardPresent(true);
  if (isNavDirection(action)) {
    moveFocus(action);
    emitAction(action, "keyboard");
    return action;
  }
  if (action === "activate") {
    activateFocused();
  }
  emitAction(action, "keyboard");
  return action;
}

let installedListener: ((event: KeyboardEvent) => void) | null = null;

/**
 * Install the production `keydown` listener on `window`. Idempotent.
 * Returns an `uninstall` callback (used by tests and the optional
 * teardown path).
 */
export function installKeyboardListener(target: Window = window): () => void {
  if (installedListener) return () => uninstallKeyboardListener(target);
  const listener = (event: KeyboardEvent) => {
    handleKeyboardEvent(event);
  };
  target.addEventListener("keydown", listener);
  installedListener = listener;
  return () => uninstallKeyboardListener(target);
}

export function uninstallKeyboardListener(target: Window = window): void {
  if (!installedListener) return;
  target.removeEventListener("keydown", installedListener);
  installedListener = null;
}
