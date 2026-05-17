import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  _resetForTests as _resetFocus,
  focusedId,
  registerFocusable,
  setFocusedId,
} from "./focus";
import {
  _resetForTests as _resetKeyboard,
  handleKeyboardEvent,
  installKeyboardListener,
  onAction,
  uninstallKeyboardListener,
} from "./keyboard";

function makeElement(rect: { left: number; top: number; width: number; height: number }): HTMLElement {
  const el = document.createElement("div");
  el.getBoundingClientRect = () => ({
    left: rect.left,
    top: rect.top,
    right: rect.left + rect.width,
    bottom: rect.top + rect.height,
    width: rect.width,
    height: rect.height,
    x: rect.left,
    y: rect.top,
    toJSON: () => ({}),
  });
  return el;
}

describe("handleKeyboardEvent", () => {
  beforeEach(() => {
    _resetFocus();
    _resetKeyboard();
  });

  it("arrow keys move focus in the requested direction", () => {
    registerFocusable({
      id: "a",
      element: makeElement({ left: 0, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "b",
      element: makeElement({ left: 200, top: 0, width: 100, height: 100 }),
    });
    setFocusedId("a");
    expect(handleKeyboardEvent({ code: "ArrowRight" })).toBe("navigate-right");
    expect(focusedId()).toBe("b");
  });

  it("Enter activates the focused element", () => {
    const onActivate = vi.fn();
    registerFocusable({
      id: "btn",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onActivate,
    });
    setFocusedId("btn");
    expect(handleKeyboardEvent({ key: "Enter" })).toBe("activate");
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it("emits non-nav actions on the action bus", () => {
    const listener = vi.fn();
    onAction(listener);
    handleKeyboardEvent({ key: "Escape" });
    handleKeyboardEvent({ key: "F10" });
    handleKeyboardEvent({ key: "/" });
    handleKeyboardEvent({ key: " " });
    expect(listener).toHaveBeenCalledTimes(4);
    expect(listener.mock.calls.map((c) => c[0])).toEqual([
      "back",
      "context",
      "search",
      "play-pause",
    ]);
    expect(listener.mock.calls.every((c) => c[1] === "keyboard")).toBe(true);
  });

  it("calls preventDefault when a key is consumed", () => {
    const preventDefault = vi.fn();
    handleKeyboardEvent({ key: "Enter", preventDefault });
    expect(preventDefault).toHaveBeenCalledOnce();
  });

  it("does NOT call preventDefault for unmapped keys", () => {
    const preventDefault = vi.fn();
    handleKeyboardEvent({ code: "KeyA", preventDefault });
    expect(preventDefault).not.toHaveBeenCalled();
  });

  it("unsubscribe from onAction stops further notifications", () => {
    const listener = vi.fn();
    const unsub = onAction(listener);
    handleKeyboardEvent({ key: "Escape" });
    unsub();
    handleKeyboardEvent({ key: "Escape" });
    expect(listener).toHaveBeenCalledOnce();
  });
});

describe("installKeyboardListener", () => {
  beforeEach(() => {
    _resetFocus();
    _resetKeyboard();
    uninstallKeyboardListener();
  });

  it("forwards window keydown events to the action bus", () => {
    const listener = vi.fn();
    const uninstall = installKeyboardListener();
    onAction(listener);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(listener).toHaveBeenCalledWith("back", "keyboard");
    uninstall();
  });

  it("uninstall removes the window listener", () => {
    const listener = vi.fn();
    const uninstall = installKeyboardListener();
    onAction(listener);
    uninstall();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(listener).not.toHaveBeenCalled();
  });

  it("is idempotent — calling install twice doesn't double-fire", () => {
    const listener = vi.fn();
    installKeyboardListener();
    installKeyboardListener();
    onAction(listener);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(listener).toHaveBeenCalledOnce();
    uninstallKeyboardListener();
  });
});
