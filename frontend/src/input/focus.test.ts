import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  _resetForTests,
  activateFocused,
  focusedId,
  getRegisteredIds,
  moveFocus,
  registerFocusable,
  setFocusedId,
  setInitialFocus,
  unregisterFocusable,
} from "./focus";

function makeElement(rect: { left: number; top: number; width: number; height: number }): HTMLElement {
  const el = document.createElement("div");
  // jsdom's `getBoundingClientRect` returns zeroes; stub it per-element.
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

describe("focus registry", () => {
  beforeEach(() => {
    _resetForTests();
  });

  it("first registered focusable becomes focused by default", () => {
    const el = makeElement({ left: 0, top: 0, width: 100, height: 100 });
    registerFocusable({ id: "a", element: el });
    expect(focusedId()).toBe("a");
  });

  it("subsequent registrations do not steal focus", () => {
    registerFocusable({
      id: "a",
      element: makeElement({ left: 0, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "b",
      element: makeElement({ left: 200, top: 0, width: 100, height: 100 }),
    });
    expect(focusedId()).toBe("a");
  });

  it("unregister moves focus to next available", () => {
    registerFocusable({
      id: "a",
      element: makeElement({ left: 0, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "b",
      element: makeElement({ left: 200, top: 0, width: 100, height: 100 }),
    });
    unregisterFocusable("a");
    expect(focusedId()).toBe("b");
    unregisterFocusable("b");
    expect(focusedId()).toBeNull();
  });

  it("registerFocusable returns an unregister callback", () => {
    const unreg = registerFocusable({
      id: "x",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
    });
    expect(getRegisteredIds()).toContain("x");
    unreg();
    expect(getRegisteredIds()).not.toContain("x");
  });

  it("setFocusedId fires onFocus / onBlur callbacks", () => {
    const focusA = vi.fn();
    const blurA = vi.fn();
    const focusB = vi.fn();
    registerFocusable({
      id: "a",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onFocus: focusA,
      onBlur: blurA,
    });
    registerFocusable({
      id: "b",
      element: makeElement({ left: 100, top: 0, width: 10, height: 10 }),
      onFocus: focusB,
    });
    // 'a' is already focused via the first-registered default, but the
    // callback didn't fire because no setFocusedId call was issued.
    // Explicitly drive a focus change.
    setFocusedId("b");
    expect(blurA).toHaveBeenCalledOnce();
    expect(focusB).toHaveBeenCalledOnce();
  });

  it("setInitialFocus is a no-op for unregistered ids", () => {
    expect(setInitialFocus("missing")).toBe(false);
    expect(focusedId()).toBeNull();
  });

  it("activateFocused fires the onActivate of the focused element", () => {
    const activate = vi.fn();
    registerFocusable({
      id: "btn",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onActivate: activate,
    });
    expect(activateFocused()).toBe(true);
    expect(activate).toHaveBeenCalledOnce();
  });

  it("activateFocused returns false when no element is focused", () => {
    expect(activateFocused()).toBe(false);
  });
});

describe("moveFocus directional navigation", () => {
  beforeEach(() => {
    _resetForTests();
  });

  function gridLayout() {
    // 3x3 grid; each tile 100×100, 20px gap.
    for (let row = 0; row < 3; row++) {
      for (let col = 0; col < 3; col++) {
        registerFocusable({
          id: `r${row}c${col}`,
          element: makeElement({
            left: col * 120,
            top: row * 120,
            width: 100,
            height: 100,
          }),
        });
      }
    }
  }

  it("navigate-right moves to the next tile in the same row", () => {
    gridLayout();
    setFocusedId("r0c0");
    expect(moveFocus("navigate-right")).toBe(true);
    expect(focusedId()).toBe("r0c1");
  });

  it("navigate-left from the leftmost tile returns false", () => {
    gridLayout();
    setFocusedId("r0c0");
    expect(moveFocus("navigate-left")).toBe(false);
    expect(focusedId()).toBe("r0c0");
  });

  it("navigate-down moves to the next row, same column", () => {
    gridLayout();
    setFocusedId("r0c1");
    expect(moveFocus("navigate-down")).toBe(true);
    expect(focusedId()).toBe("r1c1");
  });

  it("navigate-up from top row returns false", () => {
    gridLayout();
    setFocusedId("r0c1");
    expect(moveFocus("navigate-up")).toBe(false);
  });

  it("prefers in-row neighbors over distant out-of-row tiles", () => {
    // Two tiles to the right of the origin: one in the same row
    // (10px gap), one on a different row (5px gap horizontally but
    // 1000px below). The same-row tile should win because of the
    // cross-axis penalty.
    registerFocusable({
      id: "origin",
      element: makeElement({ left: 0, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "same-row-far",
      element: makeElement({ left: 200, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "wrong-row-near",
      element: makeElement({ left: 110, top: 1000, width: 100, height: 100 }),
    });
    setFocusedId("origin");
    expect(moveFocus("navigate-right")).toBe(true);
    expect(focusedId()).toBe("same-row-far");
  });

  it("moveFocus with no current focus picks the first registered focusable", () => {
    registerFocusable({
      id: "x",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
    });
    setFocusedId(null);
    expect(moveFocus("navigate-right")).toBe(true);
    expect(focusedId()).toBe("x");
  });
});
