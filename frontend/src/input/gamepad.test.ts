import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  _resetForTests as _resetFocus,
  focusedId,
  registerFocusable,
  setFocusedId,
} from "./focus";
import { _resetForTests as _resetKeyboard, onAction } from "./keyboard";
import {
  _resetForTests as _resetGamepad,
  pollGamepadsOnce,
} from "./gamepad";

import { GAMEPAD_BUTTONS } from "./keymap";

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

function makePad(pressed: number[]): Gamepad {
  const buttons = Array.from({ length: 18 }, (_, i) => ({
    pressed: pressed.includes(i),
    touched: pressed.includes(i),
    value: pressed.includes(i) ? 1 : 0,
  }));
  return {
    index: 0,
    id: "test-pad",
    connected: true,
    timestamp: 0,
    mapping: "standard",
    axes: [0, 0, 0, 0],
    buttons,
    vibrationActuator: null,
    hapticActuators: [],
  } as unknown as Gamepad;
}

describe("pollGamepadsOnce", () => {
  beforeEach(() => {
    _resetFocus();
    _resetKeyboard();
    _resetGamepad();
  });

  it("rising edge on A activates the focused element", () => {
    const onActivate = vi.fn();
    registerFocusable({
      id: "btn",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onActivate,
    });
    setFocusedId("btn");
    // First poll with A NOT pressed seeds the previous state.
    pollGamepadsOnce([makePad([])]);
    // Next poll with A pressed is the rising edge.
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it("held buttons do not re-fire", () => {
    const onActivate = vi.fn();
    registerFocusable({
      id: "btn",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onActivate,
    });
    setFocusedId("btn");
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it("DpadRight emits navigate-right and moves focus", () => {
    registerFocusable({
      id: "a",
      element: makeElement({ left: 0, top: 0, width: 100, height: 100 }),
    });
    registerFocusable({
      id: "b",
      element: makeElement({ left: 200, top: 0, width: 100, height: 100 }),
    });
    setFocusedId("a");
    pollGamepadsOnce([makePad([])]);
    const emitted = pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.DpadRight])]);
    expect(emitted).toContain("navigate-right");
    expect(focusedId()).toBe("b");
  });

  it("emits actions on the action bus with source=gamepad", () => {
    const listener = vi.fn();
    onAction(listener);
    pollGamepadsOnce([makePad([])]);
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.B])]);
    expect(listener).toHaveBeenCalledWith("back", "gamepad");
  });

  it("releasing then re-pressing re-fires (rising edge re-armed)", () => {
    const onActivate = vi.fn();
    registerFocusable({
      id: "btn",
      element: makeElement({ left: 0, top: 0, width: 10, height: 10 }),
      onActivate,
    });
    setFocusedId("btn");
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    pollGamepadsOnce([makePad([])]);
    pollGamepadsOnce([makePad([GAMEPAD_BUTTONS.A])]);
    expect(onActivate).toHaveBeenCalledTimes(2);
  });

  it("empty pad list is a no-op", () => {
    const emitted = pollGamepadsOnce([]);
    expect(emitted).toEqual([]);
  });
});
