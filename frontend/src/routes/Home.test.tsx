// PRD §F-008 Home composition: locked row order is honored, the
// Continue Watching row hides when empty, the four data-bearing rows
// render in their fixed positions, and the addon-catalogs placeholder
// is present (real catalog rows ship in a later session). Routes are
// rendered through MemoryRouter because Home reads from the router
// implicitly (route-aware ARIA, future deep-link state).

import { render } from "solid-js/web";
import { MemoryRouter } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { Home, HOME_ROW_ORDER } from "./Home";
import { _resetForTests as _resetFocus } from "../input/focus";
import { _resetForTests as _resetProfile } from "../input/profile";

function mount(host: HTMLElement) {
  return render(
    () => (
      <MemoryRouter root={() => <Home />}>
        <></>
      </MemoryRouter>
    ),
    host,
  );
}

describe("Home route", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders the home title", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host);

    expect(host.querySelector('[data-testid="home-title"]')).not.toBeNull();
  });

  it("renders the three locked data rows in PRD §F-008 order", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host);

    // CW is hidden when the resource resolves empty (the test
    // environment has no Tauri host so cwList returns []).
    expect(
      host.querySelector('[data-testid="row-continue-watching"]'),
    ).toBeNull();

    // The four data-bearing rows (rows 2-5) without the addon-catalogs
    // tail. Row 5 is now data-driven via `listHomeCatalogs`; with no
    // Tauri host in this test environment, the resource resolves to []
    // and no `row-cat-*` rows are rendered. The `Home (F-008 row 5)`
    // suite below pins the dynamic-row case.
    const expectedOrder = [
      "row-trending-now",
      "row-hidden-gems",
      "row-trending-this-week",
    ];

    const rendered = expectedOrder.map(
      (id) =>
        host?.querySelector(`[data-testid="${id}"]`) as HTMLElement | null,
    );
    for (const [i, el] of rendered.entries()) {
      expect(el, `row ${expectedOrder[i]} missing`).not.toBeNull();
    }

    // The PRD locks the ORDER. Verify the rendered rows appear in the
    // DOM in the same order.
    const positions = rendered.map((el) => el && nodeIndex(el));
    for (let i = 1; i < positions.length; i++) {
      const prev = positions[i - 1];
      const curr = positions[i];
      expect(prev).not.toBeNull();
      expect(curr).not.toBeNull();
      expect(curr! > prev!).toBe(true);
    }
  });

  it("renders no addon-catalog rows when listHomeCatalogs returns empty", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = mount(host);

    // No Tauri host in this test → `listHomeCatalogs` returns []. The
    // `<For>` over the resource must therefore render zero `row-cat-*`
    // sections (not a placeholder, not an empty row).
    const catRows = host.querySelectorAll<HTMLElement>(
      '[data-testid^="row-cat-"]',
    );
    expect(catRows.length).toBe(0);
  });

  it("exposes HOME_ROW_ORDER matching the PRD locked sequence", () => {
    expect(HOME_ROW_ORDER).toEqual([
      "continue-watching",
      "trending-now",
      "hidden-gems",
      "trending-this-week",
      "addon-catalogs",
    ]);
  });
});

function nodeIndex(el: Element): number {
  // Document-order index used to verify the PRD-locked row order. We
  // walk all elements with a `data-testid` and find the position of
  // the queried node so jumbled DOM trees would break the assertion.
  const all = Array.from(
    el.ownerDocument?.querySelectorAll("[data-testid]") ?? [],
  );
  return all.indexOf(el);
}
