import { describe, expect, it } from "vitest";

import {
  GAMEPAD_BUTTONS,
  gamepadButtonToAction,
  keyboardEventToAction,
  showsFocusRing,
} from "./keymap";

describe("keyboardEventToAction", () => {
  it("maps arrow keys via KeyboardEvent.code", () => {
    expect(keyboardEventToAction({ code: "ArrowUp" })).toBe("navigate-up");
    expect(keyboardEventToAction({ code: "ArrowDown" })).toBe("navigate-down");
    expect(keyboardEventToAction({ code: "ArrowLeft" })).toBe("navigate-left");
    expect(keyboardEventToAction({ code: "ArrowRight" })).toBe("navigate-right");
  });

  it("maps Enter -> activate, Escape -> back", () => {
    expect(keyboardEventToAction({ key: "Enter" })).toBe("activate");
    expect(keyboardEventToAction({ key: "Escape" })).toBe("back");
  });

  it("maps `/` (slash) to search per PRD §F-017 Linux KBM column", () => {
    expect(keyboardEventToAction({ key: "/" })).toBe("search");
    expect(keyboardEventToAction({ code: "Slash" })).toBe("search");
  });

  it("maps Space to play-pause", () => {
    expect(keyboardEventToAction({ key: " " })).toBe("play-pause");
    expect(keyboardEventToAction({ code: "Space" })).toBe("play-pause");
  });

  it("ignores keys with modifiers (chord-shortcuts pass through)", () => {
    expect(
      keyboardEventToAction({ code: "ArrowDown", ctrlKey: true }),
    ).toBeNull();
    expect(keyboardEventToAction({ key: "Enter", metaKey: true })).toBeNull();
    expect(keyboardEventToAction({ key: " ", altKey: true })).toBeNull();
  });

  it("returns null for unmapped keys", () => {
    expect(keyboardEventToAction({ code: "KeyA" })).toBeNull();
    expect(keyboardEventToAction({ code: "Tab" })).toBeNull();
  });
});

describe("gamepadButtonToAction", () => {
  it("maps the standard Xbox / DualShock indices to PRD §F-017 actions", () => {
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.A)).toBe("activate");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.B)).toBe("back");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.Y)).toBe("context");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.Start)).toBe("play-pause");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.DpadUp)).toBe("navigate-up");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.DpadDown)).toBe("navigate-down");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.DpadLeft)).toBe("navigate-left");
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.DpadRight)).toBe("navigate-right");
  });

  it("returns null for unmapped button indices (e.g. X / shoulders)", () => {
    expect(gamepadButtonToAction(GAMEPAD_BUTTONS.X)).toBeNull();
    expect(gamepadButtonToAction(99)).toBeNull();
  });
});

describe("showsFocusRing", () => {
  it("shows focus ring on dpad / gamepad / kbm profiles", () => {
    expect(showsFocusRing("dpad")).toBe(true);
    expect(showsFocusRing("gamepad")).toBe(true);
    expect(showsFocusRing("kbm")).toBe(true);
  });

  it("hides focus ring on touch profile (PRD §F-017 touch column)", () => {
    expect(showsFocusRing("touch")).toBe(false);
  });
});
