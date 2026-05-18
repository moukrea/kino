// PRD §F-014: BufferOverlay component behavior.
//
// Acceptance criteria pinned by these tests:
//
//   - Overlay hidden while `state === "safe"`.
//   - Overlay rendered with progressbar when state is `needsPrebuffer`
//     or `rebuffer`.
//   - Progress is `piecesAheadSeconds / requiredPrebufferS`, clamped.
//   - Stale events for a different token are ignored.

import { render } from "solid-js/web";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import {
  bufferProgress,
  BufferOverlay,
  formatEta,
  formatRate,
  REBUFFER_RECOVERY_TARGET_S,
} from "./BufferOverlay";
import type { BufferStatusEvent } from "../lib/tauri";

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return {
    ...actual,
    hasTauri: () => true,
    bufferStatus: vi.fn().mockResolvedValue(null),
    onBufferStatus: vi.fn(),
  };
});

import * as tauri from "../lib/tauri";

const mockedOnBufferStatus = vi.mocked(tauri.onBufferStatus);
const mockedBufferStatus = vi.mocked(tauri.bufferStatus);

type Listener = (s: BufferStatusEvent) => void;

function safe(token = "t-1"): BufferStatusEvent {
  return {
    token,
    state: "safe",
    requiredPrebufferS: null,
    dlRateBytesPerS: 5_000_000,
    piecesAheadSeconds: 120,
    bytesDownloaded: 50_000_000,
    fileSizeBytes: 1_000_000_000,
    positionS: 30,
    durationS: 3600,
    etaSeconds: 200,
  };
}

function prebuffer(token = "t-1"): BufferStatusEvent {
  return {
    token,
    state: "needsPrebuffer",
    requiredPrebufferS: 15,
    dlRateBytesPerS: 100_000,
    piecesAheadSeconds: 4,
    bytesDownloaded: 100_000,
    fileSizeBytes: 1_000_000_000,
    positionS: 0,
    durationS: 3600,
    etaSeconds: 10_000,
  };
}

function rebuffer(token = "t-1"): BufferStatusEvent {
  return {
    token,
    state: "rebuffer",
    requiredPrebufferS: null,
    dlRateBytesPerS: 500_000,
    piecesAheadSeconds: 3,
    bytesDownloaded: 500_000_000,
    fileSizeBytes: 1_000_000_000,
    positionS: 1800,
    durationS: 3600,
    etaSeconds: 1000,
  };
}

describe("BufferOverlay", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;
  let pushEvent: Listener = () => {};
  let unlistenSpy = vi.fn();

  beforeEach(() => {
    mockedBufferStatus.mockReset().mockResolvedValue(null);
    unlistenSpy = vi.fn();
    mockedOnBufferStatus.mockReset().mockImplementation((cb) => {
      pushEvent = cb;
      return Promise.resolve(unlistenSpy);
    });
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("renders nothing while state is safe", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    mockedBufferStatus.mockResolvedValue(safe("t-1"));
    dispose = render(() => <BufferOverlay token="t-1" />, host);

    // Wait for the async onMount to complete.
    await Promise.resolve();
    await Promise.resolve();

    expect(host.querySelector(".buffer-overlay")).toBeNull();
  });

  it("renders the prebuffer overlay when a needsPrebuffer event arrives", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <BufferOverlay token="t-1" />, host);

    await Promise.resolve();
    await Promise.resolve();
    pushEvent(prebuffer("t-1"));

    const overlay = host.querySelector(".buffer-overlay");
    expect(overlay).not.toBeNull();
    const progressbar = host.querySelector('[role="progressbar"]');
    expect(progressbar).not.toBeNull();
    // 4 / 15 ≈ 0.266…
    expect(progressbar?.getAttribute("aria-valuenow")).toMatch(/^0\.26/);
  });

  it("renders the rebuffer heading when state is rebuffer", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <BufferOverlay token="t-1" />, host);

    await Promise.resolve();
    await Promise.resolve();
    pushEvent(rebuffer("t-1"));

    const heading = host.querySelector(".buffer-overlay__title");
    expect(heading?.textContent).toMatch(/Rebuffering|tampon/i);
  });

  it("ignores events for a different token", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <BufferOverlay token="t-1" />, host);

    await Promise.resolve();
    await Promise.resolve();
    pushEvent(prebuffer("OTHER"));

    expect(host.querySelector(".buffer-overlay")).toBeNull();
  });

  it("unsubscribes on unmount", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    dispose = render(() => <BufferOverlay token="t-1" />, host);

    await Promise.resolve();
    await Promise.resolve();
    dispose?.();
    dispose = null;

    expect(unlistenSpy).toHaveBeenCalledOnce();
  });
});

describe("bufferProgress", () => {
  it("returns 1 for safe state", () => {
    expect(bufferProgress(safe())).toBe(1);
  });

  it("returns piecesAhead / required for needsPrebuffer", () => {
    const e = prebuffer();
    const expected = e.piecesAheadSeconds / (e.requiredPrebufferS ?? 1);
    expect(bufferProgress(e)).toBeCloseTo(expected, 4);
  });

  it("clamps progress to [0, 1]", () => {
    const high: BufferStatusEvent = {
      ...prebuffer(),
      piecesAheadSeconds: 1000,
      requiredPrebufferS: 10,
    };
    expect(bufferProgress(high)).toBe(1);

    const negative: BufferStatusEvent = {
      ...prebuffer(),
      piecesAheadSeconds: 0,
      requiredPrebufferS: 10,
    };
    expect(bufferProgress(negative)).toBe(0);
  });

  it("uses REBUFFER_RECOVERY_TARGET_S for the rebuffer branch", () => {
    const r: BufferStatusEvent = {
      ...rebuffer(),
      piecesAheadSeconds: REBUFFER_RECOVERY_TARGET_S * 0.5,
    };
    expect(bufferProgress(r)).toBeCloseTo(0.5, 4);
  });

  it("returns 0 when status is null", () => {
    expect(bufferProgress(null)).toBe(0);
  });
});

describe("formatRate", () => {
  it("formats MB/s", () => {
    expect(formatRate(5_500_000)).toBe("5.2 MB");
  });
  it("formats KB/s", () => {
    expect(formatRate(123_000)).toBe("120 KB");
  });
  it("formats B/s", () => {
    expect(formatRate(456)).toBe("456 B");
  });
  it("guards non-finite", () => {
    expect(formatRate(Number.NaN)).toBe("0 B");
    expect(formatRate(-1)).toBe("0 B");
  });
});

describe("formatEta", () => {
  it("returns unknown for null", () => {
    const out = formatEta(null);
    expect(out).toMatch(/unknown|inconnu/i);
  });
  it("formats seconds", () => {
    expect(formatEta(45)).toBe("45s");
  });
  it("formats minutes + seconds", () => {
    expect(formatEta(125)).toBe("2m 05s");
  });
  it("formats hours + minutes", () => {
    expect(formatEta(7320)).toBe("2h 02m");
  });
});
