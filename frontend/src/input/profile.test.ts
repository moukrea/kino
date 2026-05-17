import { beforeEach, describe, expect, it } from "vitest";

import {
  _resetForTests,
  defaultProfileForPlatform,
  reportGamepadPresent,
  reportTouchPresent,
  resolveProfile,
  setOverride,
  setPlatform,
  profile,
  type Capabilities,
} from "./profile";

const NO_DEVICES: Capabilities = {
  hasTouch: false,
  hasGamepad: false,
  hasKeyboard: true,
};

describe("defaultProfileForPlatform", () => {
  it("locks Android TV primary input to dpad (PRD §F-017)", () => {
    expect(defaultProfileForPlatform("android-tv")).toBe("dpad");
  });

  it("locks Android mobile primary input to touch (PRD §F-017)", () => {
    expect(defaultProfileForPlatform("android-mobile")).toBe("touch");
  });

  it("locks Linux primary input to kbm (PRD §F-017)", () => {
    expect(defaultProfileForPlatform("linux")).toBe("kbm");
  });
});

describe("resolveProfile", () => {
  it("user override always wins (non-auto)", () => {
    expect(
      resolveProfile("android-tv", { ...NO_DEVICES, hasTouch: true }, "kbm"),
    ).toBe("kbm");
    expect(resolveProfile("linux", NO_DEVICES, "touch")).toBe("touch");
    expect(resolveProfile("android-mobile", NO_DEVICES, "gamepad")).toBe(
      "gamepad",
    );
  });

  it("auto resolves Android TV to dpad regardless of attached keyboard", () => {
    expect(
      resolveProfile("android-tv", { ...NO_DEVICES, hasKeyboard: true }, "auto"),
    ).toBe("dpad");
  });

  it("auto resolves Android mobile to touch by default", () => {
    expect(resolveProfile("android-mobile", NO_DEVICES, "auto")).toBe("touch");
  });

  it("auto upgrades Android mobile to gamepad when a gamepad is present", () => {
    expect(
      resolveProfile(
        "android-mobile",
        { ...NO_DEVICES, hasGamepad: true },
        "auto",
      ),
    ).toBe("gamepad");
  });

  it("auto resolves Linux to kbm by default", () => {
    expect(resolveProfile("linux", NO_DEVICES, "auto")).toBe("kbm");
  });

  it("auto upgrades Linux to gamepad only when keyboard is gone", () => {
    expect(
      resolveProfile(
        "linux",
        { hasKeyboard: false, hasGamepad: true, hasTouch: false },
        "auto",
      ),
    ).toBe("gamepad");
    expect(
      resolveProfile(
        "linux",
        { hasKeyboard: true, hasGamepad: true, hasTouch: false },
        "auto",
      ),
    ).toBe("kbm");
  });
});

describe("profile signal", () => {
  beforeEach(() => {
    _resetForTests();
  });

  it("reacts to platform changes when override is auto", () => {
    setPlatform("linux");
    expect(profile()).toBe("kbm");
    setPlatform("android-tv");
    expect(profile()).toBe("dpad");
  });

  it("reacts to a runtime gamepad connect on android-mobile", () => {
    setPlatform("android-mobile");
    expect(profile()).toBe("touch");
    reportGamepadPresent(true);
    expect(profile()).toBe("gamepad");
    reportGamepadPresent(false);
    expect(profile()).toBe("touch");
  });

  it("override pins the profile against capability flips", () => {
    setPlatform("android-mobile");
    setOverride("kbm");
    reportGamepadPresent(true);
    expect(profile()).toBe("kbm");
    reportTouchPresent(true);
    expect(profile()).toBe("kbm");
  });
});
