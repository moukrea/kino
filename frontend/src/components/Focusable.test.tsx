import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Focusable, LONG_PRESS_MS } from "./Focusable";
import {
  _resetForTests as _resetFocus,
  focusedId,
  setFocusedId,
} from "../input/focus";
import { emitAction, _resetForTests as _resetKeyboard } from "../input/keyboard";
import { _resetForTests as _resetProfile, setOverride } from "../input/profile";

describe("Focusable", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    _resetKeyboard();
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

  it("right-click fires onContext and suppresses the browser default menu (PRD §F-012 / §F-017)", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const onContext = vi.fn();
    dispose = render(
      () => (
        <Focusable id="cw-tile" onContext={onContext}>
          {({ ref, onContextMenu }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              onContextMenu={onContextMenu}
              data-testid="cw-tile"
            >
              tile
            </button>
          )}
        </Focusable>
      ),
      host,
    );
    const tile = host.querySelector('[data-testid="cw-tile"]') as HTMLButtonElement;
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    tile.dispatchEvent(event);
    expect(onContext).toHaveBeenCalledOnce();
    expect(event.defaultPrevented).toBe(true);
  });

  it("emits onContext when the `context` action fires while this tile is focused", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const onContextA = vi.fn();
    const onContextB = vi.fn();
    dispose = render(
      () => (
        <>
          <Focusable id="a" onContext={onContextA}>
            {({ ref }) => (
              <button ref={ref as (el: HTMLButtonElement) => void} data-testid="a">
                a
              </button>
            )}
          </Focusable>
          <Focusable id="b" onContext={onContextB}>
            {({ ref }) => (
              <button ref={ref as (el: HTMLButtonElement) => void} data-testid="b">
                b
              </button>
            )}
          </Focusable>
        </>
      ),
      host,
    );
    // 'a' is focused by default.
    expect(focusedId()).toBe("a");
    emitAction("context", "keyboard");
    expect(onContextA).toHaveBeenCalledOnce();
    expect(onContextB).not.toHaveBeenCalled();
    setFocusedId("b");
    emitAction("context", "gamepad");
    expect(onContextB).toHaveBeenCalledOnce();
    // 'a' should NOT have been called a second time.
    expect(onContextA).toHaveBeenCalledOnce();
  });

  it("long-press on touch fires onContext after LONG_PRESS_MS", async () => {
    vi.useFakeTimers();
    try {
      host = document.createElement("div");
      document.body.appendChild(host);
      const onContext = vi.fn();
      const onActivate = vi.fn();
      dispose = render(
        () => (
          <Focusable id="cw-tile" onContext={onContext} onActivate={onActivate}>
            {({ ref, onClick, onTouchStart, onTouchEnd, onTouchCancel }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                onTouchStart={onTouchStart}
                onTouchEnd={onTouchEnd}
                onTouchCancel={onTouchCancel}
                data-testid="cw-tile"
              >
                tile
              </button>
            )}
          </Focusable>
        ),
        host,
      );
      const tile = host.querySelector('[data-testid="cw-tile"]') as HTMLButtonElement;
      tile.dispatchEvent(new Event("touchstart", { bubbles: true }));
      vi.advanceTimersByTime(LONG_PRESS_MS);
      expect(onContext).toHaveBeenCalledOnce();
      // A trailing click (synthesized by the browser after the
      // long-press) must NOT additionally activate the tile.
      tile.click();
      expect(onActivate).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("touch tap < LONG_PRESS_MS does NOT fire onContext", async () => {
    vi.useFakeTimers();
    try {
      host = document.createElement("div");
      document.body.appendChild(host);
      const onContext = vi.fn();
      dispose = render(
        () => (
          <Focusable id="cw-tile" onContext={onContext}>
            {({ ref, onTouchStart, onTouchEnd }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onTouchStart={onTouchStart}
                onTouchEnd={onTouchEnd}
                data-testid="cw-tile"
              >
                tile
              </button>
            )}
          </Focusable>
        ),
        host,
      );
      const tile = host.querySelector('[data-testid="cw-tile"]') as HTMLButtonElement;
      tile.dispatchEvent(new Event("touchstart", { bubbles: true }));
      vi.advanceTimersByTime(100);
      tile.dispatchEvent(new Event("touchend", { bubbles: true }));
      vi.advanceTimersByTime(LONG_PRESS_MS);
      expect(onContext).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does NOT subscribe to onContext routing when onContext prop is absent", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(
      () => (
        <Focusable id="plain">
          {({ ref, onContextMenu }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              onContextMenu={onContextMenu}
              data-testid="plain"
            >
              plain
            </button>
          )}
        </Focusable>
      ),
      host,
    );
    const tile = host.querySelector('[data-testid="plain"]') as HTMLButtonElement;
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    tile.dispatchEvent(event);
    // No handler set → the browser default is NOT prevented.
    expect(event.defaultPrevented).toBe(false);
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
