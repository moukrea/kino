// Smoke tests for the F-008 App shell. The router, nav rail, and
// installed input subsystem are exercised here at the integration
// boundary; per-component behavior (focus, virtualization, 600ms info
// overlay) is covered by the component-level test files.

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import App from "./App";
import { _resetForTests as _resetFocus } from "./input/focus";
import {
  _resetForTests as _resetKeyboard,
  uninstallKeyboardListener,
} from "./input/keyboard";
import { _resetForTests as _resetProfile, setOverride } from "./input/profile";

describe("App", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetKeyboard();
    _resetProfile();
    uninstallKeyboardListener();
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
    uninstallKeyboardListener();
  });

  it("mounts the shell with the nav rail visible", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    expect(host.querySelector('[data-testid="app-shell"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="nav-rail"]')).not.toBeNull();
  });

  it("renders all five PRD §F-008 nav rail items", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    for (const id of ["home", "movies", "series", "search", "settings"]) {
      const item = host.querySelector(`[data-testid="nav-item-${id}"]`);
      expect(item, `nav item ${id} missing`).not.toBeNull();
    }
  });

  it("renders the home route at /", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    expect(host.querySelector('[data-testid="home-title"]')).not.toBeNull();
  });

  it("installs the input subsystem so keyboard events drive focus", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(() => <App />, host);

    // The nav rail's home item is the first focusable to register;
    // the focus manager picks it as the default focus. Pressing
    // ArrowDown should move focus to the next nav item ("movies").
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" }));

    const moviesItem = host.querySelector(
      '[data-testid="nav-item-movies"]',
    ) as HTMLElement | null;
    expect(moviesItem?.dataset.focused).toBe("true");
  });
});
