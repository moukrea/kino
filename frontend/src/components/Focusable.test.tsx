import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Focusable } from "./Focusable";
import {
  _resetForTests as _resetFocus,
  focusedId,
  setFocusedId,
} from "../input/focus";
import { _resetForTests as _resetProfile, setOverride } from "../input/profile";

describe("Focusable", () => {
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

  it("registers its element and becomes focused by default", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Focusable id="tile-1">
          {({ ref, focused }) => (
            <div
              ref={ref}
              data-testid="tile-1"
              data-focused={focused() ? "yes" : "no"}
            >
              tile-1
            </div>
          )}
        </Focusable>
      ),
      host,
    );
    expect(focusedId()).toBe("tile-1");
    const node = host.querySelector('[data-testid="tile-1"]');
    expect(node?.getAttribute("data-focused")).toBe("yes");
  });

  it("click handler claims focus and fires onActivate", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const onActivate = vi.fn();
    dispose = render(
      () => (
        <>
          <Focusable id="a">
            {({ ref, onClick }) => (
              <button ref={ref as (el: HTMLButtonElement) => void} onClick={onClick}>
                a
              </button>
            )}
          </Focusable>
          <Focusable id="b" onActivate={onActivate}>
            {({ ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="b"
              >
                b
              </button>
            )}
          </Focusable>
        </>
      ),
      host,
    );
    expect(focusedId()).toBe("a");
    (host.querySelector('[data-testid="b"]') as HTMLButtonElement).click();
    expect(focusedId()).toBe("b");
    expect(onActivate).toHaveBeenCalledOnce();
  });

  it("focus ring is hidden under the touch profile", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("touch");
    dispose = render(
      () => (
        <Focusable id="tile">
          {({ ref, showRing }) => (
            <div ref={ref} data-ring={showRing() ? "yes" : "no"} data-testid="tile">
              tile
            </div>
          )}
        </Focusable>
      ),
      host,
    );
    setFocusedId("tile");
    const node = host.querySelector('[data-testid="tile"]');
    expect(node?.getAttribute("data-ring")).toBe("no");
  });

  it("focus ring is shown under the kbm profile", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    setOverride("kbm");
    dispose = render(
      () => (
        <Focusable id="tile">
          {({ ref, showRing }) => (
            <div ref={ref} data-ring={showRing() ? "yes" : "no"} data-testid="tile">
              tile
            </div>
          )}
        </Focusable>
      ),
      host,
    );
    setFocusedId("tile");
    const node = host.querySelector('[data-testid="tile"]');
    expect(node?.getAttribute("data-ring")).toBe("yes");
  });
});
