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

  it("renders the placeholder home title required by PRD F-001", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    const title = host.querySelector('[data-testid="home-title"]');
    expect(title?.textContent).toBe("kino");
  });

  it("renders a tagline alongside the title", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    const tagline = host.querySelector('[data-testid="home-tagline"]');
    expect(tagline?.textContent).toBeTruthy();
  });

  it("renders the F-017 input demonstrator with three demo tiles", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <App />, host);

    expect(host.querySelector('[data-testid="demo-tile-1"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="demo-tile-2"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="demo-tile-3"]')).not.toBeNull();
  });

  it("keyboard ArrowRight updates the displayed last-action", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(() => <App />, host);

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight" }));

    const lastAction = host.querySelector(
      '[data-testid="input-last-action"]',
    );
    expect(lastAction?.textContent).toContain("navigate-right");
    expect(lastAction?.textContent).toContain("keyboard");
  });

  it("Escape key surfaces as 'back' on the action bus", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(() => <App />, host);

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));

    const lastAction = host.querySelector(
      '[data-testid="input-last-action"]',
    );
    expect(lastAction?.textContent).toContain("back");
  });
});
