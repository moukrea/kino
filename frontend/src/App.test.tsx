// Smoke tests for the F-008 App shell. The router, nav rail, and
// installed input subsystem are exercised here at the integration
// boundary; per-component behavior (focus, virtualization, 600ms info
// overlay) is covered by the component-level test files.

import { ErrorBoundary, type Component } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import App, { RootErrorFallback } from "./App";
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

  it("the root ErrorBoundary catches render errors and renders the fallback (PRD §5 Reliability)", () => {
    // The root boundary wraps the SolidJS <Router> in App.tsx. We can't
    // easily make a routed component throw from a test without re-mounting
    // App with a stubbed route, so we exercise the same boundary contract
    // here directly: an ErrorBoundary using App's exported fallback catches
    // a render-time throw, paints the testable fallback markup, and logs
    // the error via console.error (the Tauri runtime relays that into the
    // `tracing` file appender).
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const Boom: Component = () => {
      throw new Error("kino test boom");
    };

    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <ErrorBoundary
          fallback={(err, reset) => (
            <RootErrorFallback error={err} reset={reset} />
          )}
        >
          <Boom />
        </ErrorBoundary>
      ),
      host,
    );

    const fallback = host.querySelector('[data-testid="app-error-boundary"]');
    expect(fallback).not.toBeNull();
    const message = host.querySelector('[data-testid="app-error-message"]');
    expect(message?.textContent ?? "").toContain("kino test boom");
    // PRD §5 "Frontend errors caught at root error boundary and logged":
    // the fallback emits via console.error.
    expect(
      errorSpy.mock.calls.some((args) =>
        args.some((a) =>
          a instanceof Error
            ? a.message.includes("kino test boom")
            : String(a).includes("kino test boom"),
        ),
      ),
    ).toBe(true);

    errorSpy.mockRestore();
  });

  it("the '/' search shortcut navigates to /search from another route (PRD §F-011)", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(() => <App />, host);

    // Start on Home (default route). Press "/" — F-017 maps it to the
    // `search` Action; App.tsx's onAction handler navigates to /search.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "/" }));
    // Solid commits the route change synchronously, but the route's
    // `onMount` autofocus is deferred via queueMicrotask.
    await Promise.resolve();
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));

    expect(host.querySelector('[data-testid="search-route"]')).not.toBeNull();
    // Home title gone, search title in.
    expect(host.querySelector('[data-testid="home-title"]')).toBeNull();
    expect(host.querySelector('[data-testid="search-title"]')).not.toBeNull();
  });
});
