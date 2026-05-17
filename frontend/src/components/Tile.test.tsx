// PRD §F-008 Tile behavior: focus claims, scale + ring on focus, and
// the 600ms info overlay arming on held focus + clearing on blur.

import { render } from "solid-js/web";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import { Tile, INFO_OVERLAY_DELAY_MS } from "./Tile";
import { _resetForTests as _resetFocus, setFocusedId } from "../input/focus";
import { _resetForTests as _resetProfile, setOverride } from "../input/profile";
import type { TitleSummary } from "../lib/tauri";

const SAMPLE: TitleSummary = {
  id: "tt0133093",
  kind: "movie",
  title: "The Matrix",
  year: 1999,
  poster: null,
  rating: 8.7,
};

describe("Tile", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders a button labeled with title and year", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => <Tile focusId="t1" summary={SAMPLE} />,
      host,
    );

    const button = host.querySelector(
      '[data-testid="tile-t1"]',
    ) as HTMLButtonElement | null;
    expect(button).not.toBeNull();
    expect(button?.getAttribute("aria-label")).toBe("The Matrix (1999)");
  });

  it("shows the title caption only when focused", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(
      () => <Tile focusId="t1" summary={SAMPLE} />,
      host,
    );

    // First focusable to register auto-claims focus; the caption must
    // be visible on this single tile.
    expect(
      host.querySelector('[data-testid="tile-caption"]'),
    ).not.toBeNull();

    // Move focus elsewhere and verify the caption disappears.
    setFocusedId(null);
    expect(host.querySelector('[data-testid="tile-caption"]')).toBeNull();
  });

  it("arms the info overlay after 600ms of held focus", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(
      () => <Tile focusId="t1" summary={SAMPLE} />,
      host,
    );

    // Auto-focused on registration. Overlay should NOT be present yet.
    expect(
      host.querySelector('[data-testid="tile-info-overlay"]'),
    ).toBeNull();

    vi.advanceTimersByTime(INFO_OVERLAY_DELAY_MS - 1);
    expect(
      host.querySelector('[data-testid="tile-info-overlay"]'),
    ).toBeNull();

    vi.advanceTimersByTime(1);
    expect(
      host.querySelector('[data-testid="tile-info-overlay"]'),
    ).not.toBeNull();
  });

  it("cancels the info overlay if focus is lost before 600ms", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(
      () => <Tile focusId="t1" summary={SAMPLE} />,
      host,
    );

    vi.advanceTimersByTime(INFO_OVERLAY_DELAY_MS - 100);
    setFocusedId(null);
    vi.advanceTimersByTime(200);
    expect(
      host.querySelector('[data-testid="tile-info-overlay"]'),
    ).toBeNull();
  });

  it("activates via click and clears any pending overlay", () => {
    const onActivate = vi.fn();
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(
      () => (
        <Tile focusId="t1" summary={SAMPLE} onActivate={onActivate} />
      ),
      host,
    );

    const button = host.querySelector(
      '[data-testid="tile-t1"]',
    ) as HTMLButtonElement;
    vi.advanceTimersByTime(INFO_OVERLAY_DELAY_MS - 100);
    button.click();

    expect(onActivate).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(200);
    expect(
      host.querySelector('[data-testid="tile-info-overlay"]'),
    ).toBeNull();
  });

  it("falls back to placeholder text when no poster URL is set", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => <Tile focusId="t1" summary={SAMPLE} />,
      host,
    );

    expect(
      host.querySelector('[data-testid="tile-poster-placeholder"]'),
    ).not.toBeNull();
  });

  it("renders an <img> when a poster URL is present", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Tile
          focusId="t1"
          summary={{ ...SAMPLE, poster: "https://example/poster.jpg" }}
        />
      ),
      host,
    );

    const img = host.querySelector("img");
    expect(img).not.toBeNull();
    expect(img?.getAttribute("src")).toBe("https://example/poster.jpg");
    expect(img?.getAttribute("loading")).toBe("lazy");
  });
});
