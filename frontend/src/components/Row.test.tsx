// PRD §F-008 Row behavior: renders label + horizontal track, hides the
// track when empty (so the consumer can hide the row entirely via
// `emptyFallback={null}`), and lazy-loads tiles past the initial
// window (virtualization acceptance).

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Row, INITIAL_WINDOW, WINDOW_STEP } from "./Row";
import { _resetForTests as _resetFocus, setFocusedId } from "../input/focus";
import { _resetForTests as _resetProfile, setOverride } from "../input/profile";
import type { TitleSummary } from "../lib/tauri";

function makeItems(count: number): TitleSummary[] {
  return Array.from({ length: count }, (_, i) => ({
    id: `t${i}`,
    kind: "movie" as const,
    title: `Title ${i}`,
    year: 2000 + i,
    poster: null,
    rating: null,
  }));
}

describe("Row", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    setOverride("kbm");
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders the label and a track of tiles", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Trending Now"
          focusIdPrefix="row-trending"
          items={makeItems(3)}
        />
      ),
      host,
    );

    expect(host.textContent).toContain("Trending Now");
    expect(host.querySelectorAll('[data-testid^="tile-row-trending-"]'))
      .toHaveLength(3);
    expect(host.querySelector('[data-testid="row-track"]')).not.toBeNull();
  });

  it("shows the default placeholder when items are empty", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row label="Empty" focusIdPrefix="row-empty" items={[]} />
      ),
      host,
    );

    expect(
      host.querySelector('[data-testid="row-empty-fallback"]'),
    ).not.toBeNull();
    expect(host.querySelector('[data-testid="row-track"]')).toBeNull();
  });

  it("renders nothing in place of the row body when emptyFallback is null", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Hidden"
          focusIdPrefix="row-hidden"
          items={[]}
          emptyFallback={null}
        />
      ),
      host,
    );

    expect(
      host.querySelector('[data-testid="row-empty-fallback"]'),
    ).toBeNull();
    expect(host.querySelector('[data-testid="row-track"]')).toBeNull();
  });

  it("only renders the initial window of tiles for large catalogs (virtualization)", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Big"
          focusIdPrefix="row-big"
          items={makeItems(100)}
        />
      ),
      host,
    );

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-big-"]',
    );
    expect(rendered.length).toBe(INITIAL_WINDOW);
  });

  it("grows the window when focus reaches the tail of the rendered set", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Big"
          focusIdPrefix="row-big"
          items={makeItems(100)}
        />
      ),
      host,
    );

    // Focus a tile near the tail (within `TAIL_TRIGGER = 3` of the
    // window edge). Default `INITIAL_WINDOW = 12`, so focusing index
    // 10 (3rd from tail) should trigger growth.
    setFocusedId(`row-big-t10`);

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-big-"]',
    );
    expect(rendered.length).toBe(INITIAL_WINDOW + WINDOW_STEP);
  });

  it("does not grow when an unrelated row's tile gains focus", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Big"
          focusIdPrefix="row-big"
          items={makeItems(100)}
        />
      ),
      host,
    );

    // Focus an id with a different prefix — must not grow this row.
    setFocusedId(`row-other-t11`);

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-big-"]',
    );
    expect(rendered.length).toBe(INITIAL_WINDOW);
  });

  it("invokes onActivate with the summary when a tile is clicked", () => {
    const onActivate = vi.fn();
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Big"
          focusIdPrefix="row-call"
          items={makeItems(3)}
          onActivate={onActivate}
        />
      ),
      host,
    );

    const tile = host.querySelector(
      '[data-testid="tile-row-call-t1"]',
    ) as HTMLButtonElement;
    tile.click();

    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onActivate.mock.calls[0]?.[0].id).toBe("t1");
  });

  it("PRD §F-006: hides unavailable tiles by default (showUnavailable OFF)", () => {
    // Mark every odd tile as unavailable; the Row should hide them
    // entirely so only the 5 even-indexed tiles render.
    const items = makeItems(10);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Mixed"
          focusIdPrefix="row-mixed"
          items={items}
          itemAvailability={(s) =>
            Number(s.id.slice(1)) % 2 === 0 ? "available" : "unavailable"
          }
        />
      ),
      host,
    );

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-mixed-"]',
    );
    // 5 even-indexed tiles survive the filter.
    expect(rendered.length).toBe(5);
    for (const el of Array.from(rendered)) {
      expect(el.getAttribute("data-availability")).toBe("available");
    }
  });

  it("PRD §F-006: shows unavailable tiles with badge when showUnavailable=true", () => {
    const items = makeItems(6);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Mixed"
          focusIdPrefix="row-mixed"
          items={items}
          showUnavailable={true}
          itemAvailability={(s) =>
            Number(s.id.slice(1)) % 2 === 0 ? "available" : "unavailable"
          }
        />
      ),
      host,
    );

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-mixed-"]',
    );
    // All 6 tiles render now.
    expect(rendered.length).toBe(6);
    // Three of them carry the no-source badge.
    expect(
      host.querySelectorAll('[data-testid="tile-no-source-badge"]').length,
    ).toBe(3);
  });

  it("PRD §F-006: keeps pending tiles visible regardless of the toggle", () => {
    // "pending" is the PRD's "availability unknown" placeholder; it
    // must stay rendered so the row reserves space for the eventual
    // result rather than collapsing while the backend resolves.
    const items = makeItems(4);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="Loading"
          focusIdPrefix="row-loading"
          items={items}
          itemAvailability={() => "pending"}
        />
      ),
      host,
    );

    const rendered = host.querySelectorAll(
      '[data-testid^="tile-row-loading-"]',
    );
    expect(rendered.length).toBe(4);
    expect(
      host.querySelectorAll('[data-testid="tile-skeleton"]').length,
    ).toBe(4);
  });

  it("PRD §F-006: an all-unavailable row collapses to the empty fallback", () => {
    // A row whose every tile is unavailable with the toggle OFF must
    // render its empty placeholder rather than an empty track.
    const items = makeItems(3);
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Row
          label="None"
          focusIdPrefix="row-none"
          items={items}
          itemAvailability={() => "unavailable"}
        />
      ),
      host,
    );

    expect(
      host.querySelector('[data-testid="row-empty-fallback"]'),
    ).not.toBeNull();
    expect(host.querySelector('[data-testid="row-track"]')).toBeNull();
  });
});
