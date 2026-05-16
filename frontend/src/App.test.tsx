import { render } from "solid-js/web";
import { afterEach, describe, expect, it } from "vitest";

import App from "./App";

describe("App", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
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
});
