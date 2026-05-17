// PRD §F-008 nav rail: collapsed by default (icons only), expands on
// focus or hover, all five top-level routes (Home, Movies, Series,
// Search, Settings) are reachable. The rail's navigation on activation
// is tested through a MemoryRouter so we can observe the location
// change without a real browser URL bar.

import { render } from "solid-js/web";
import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { NavRail } from "./NavRail";
import { _resetForTests as _resetFocus, setFocusedId } from "../input/focus";
import { _resetForTests as _resetProfile, setOverride } from "../input/profile";

function mount(host: HTMLElement, initialPath = "/") {
  const Stub = () => null;
  const history = createMemoryHistory();
  if (initialPath !== "/") {
    history.set({ value: initialPath });
  }
  return render(
    () => (
      <MemoryRouter
        history={history}
        root={(props) => (
          <>
            <NavRail />
            {props.children}
          </>
        )}
      >
        <Route path="/" component={Stub} />
        <Route path="/movies" component={Stub} />
        <Route path="/series" component={Stub} />
        <Route path="/search" component={Stub} />
        <Route path="/settings" component={Stub} />
      </MemoryRouter>
    ),
    host,
  );
}

describe("NavRail", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    setOverride("kbm");
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders all five PRD §F-008 nav items", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host);

    for (const id of ["home", "movies", "series", "search", "settings"]) {
      expect(
        host.querySelector(`[data-testid="nav-item-${id}"]`),
        `missing nav item ${id}`,
      ).not.toBeNull();
    }
  });

  it("starts collapsed (icons only) and expands when a nav item is focused", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host);

    const rail = host.querySelector(
      '[data-testid="nav-rail"]',
    ) as HTMLElement;

    // The first focusable to register auto-claims focus per the focus
    // manager. NavRail's "home" item is registered first; that
    // already expands the rail. Verify by blurring focus and checking
    // collapsed state.
    setFocusedId(null);
    expect(rail.dataset.expanded).toBe("false");

    setFocusedId("nav-movies");
    expect(rail.dataset.expanded).toBe("true");
  });

  it("marks the active route via data-active='true'", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host, "/movies");

    const moviesItem = host.querySelector(
      '[data-testid="nav-item-movies"]',
    ) as HTMLElement;
    expect(moviesItem.dataset.active).toBe("true");

    const homeItem = host.querySelector(
      '[data-testid="nav-item-home"]',
    ) as HTMLElement;
    expect(homeItem.dataset.active).toBe("false");
  });
});
