// PRD §F-015 Player route tests. Pin behavior the route is responsible
// for at the SolidJS layer:
//
//   - Pops back when reached without session state (route is not
//     directly addressable).
//   - Boots the F-015 driver via `playerOpen` with the navigation-state
//     payload AND starts the F-014 buffer monitor with the same token.
//   - Subscribes to `player:*` events and reflects them in the UI
//     (state badge, position, paused, tracks list).
//   - Pause / Seek / Audio / Subtitle buttons dispatch to the matching
//     Tauri command.
//   - `Esc` / B / back-button tears down the session (`playerClose` +
//     `stopPlayback`) before navigating back.
//   - Terminal `Exit` event auto-tears-down + pops back.
//
// The mpv subprocess itself is not exercised (ADR-108); the route
// integration boundary is the Tauri command surface.

import { render } from "solid-js/web";
import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Player, formatTime, trackLabel } from "./Player";
import { _resetForTests as _resetFocus } from "../input/focus";
import { _resetForTests as _resetProfile } from "../input/profile";
import { emitAction } from "../input/keyboard";
import {
  _resetForTests as _resetPlayerSession,
  setPlayerSession,
  type PlayerSessionState,
} from "../lib/playerSession";
import type {
  AudioTrack,
  PlayerEvent,
  PlayerStatusResponse,
  SubtitleTrack,
} from "../lib/tauri";

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return {
    ...actual,
    hasTauri: () => true,
    playerOpen: vi.fn(async () => undefined),
    playerClose: vi.fn(async () => true),
    playerPause: vi.fn(async () => undefined),
    playerSeek: vi.fn(async () => undefined),
    playerSetAudioTrack: vi.fn(async () => undefined),
    playerSetSubtitleTrack: vi.fn(async () => undefined),
    playerStatus: vi.fn(async () => null),
    bufferStartMonitor: vi.fn(async () => undefined),
    bufferStopMonitor: vi.fn(async () => true),
    stopPlayback: vi.fn(async () => true),
    onPlayerPosition: vi.fn(),
    onPlayerState: vi.fn(),
    onPlayerTracks: vi.fn(),
    onPlayerExit: vi.fn(),
    onPlayerError: vi.fn(),
    onBufferStatus: vi.fn(),
    bufferStatus: vi.fn(async () => null),
  };
});

const tauri = await import("../lib/tauri");
const mockedPlayerOpen = vi.mocked(tauri.playerOpen);
const mockedPlayerClose = vi.mocked(tauri.playerClose);
const mockedPlayerPause = vi.mocked(tauri.playerPause);
const mockedPlayerSeek = vi.mocked(tauri.playerSeek);
const mockedPlayerSetAudio = vi.mocked(tauri.playerSetAudioTrack);
const mockedPlayerSetSub = vi.mocked(tauri.playerSetSubtitleTrack);
const mockedPlayerStatus = vi.mocked(tauri.playerStatus);
const mockedBufferStart = vi.mocked(tauri.bufferStartMonitor);
const mockedBufferStop = vi.mocked(tauri.bufferStopMonitor);
const mockedStopPlayback = vi.mocked(tauri.stopPlayback);
const mockedOnPosition = vi.mocked(tauri.onPlayerPosition);
const mockedOnState = vi.mocked(tauri.onPlayerState);
const mockedOnTracks = vi.mocked(tauri.onPlayerTracks);
const mockedOnExit = vi.mocked(tauri.onPlayerExit);
const mockedOnError = vi.mocked(tauri.onPlayerError);

function makeSession(
  overrides: Partial<PlayerSessionState> = {},
): PlayerSessionState {
  return {
    token: "session-token-uuid",
    url: "http://127.0.0.1:54321/stream/abc",
    resumePositionS: 0,
    fileName: "The.Matrix.1999.mkv",
    durationHintS: 8160,
    cwContext: {
      titleId: "imdb:tt0133093",
      kind: "movie",
      season: 0,
      episode: 0,
      metaJson: { title: "The Matrix" },
      episodes: [],
    },
    displayTitle: "The Matrix",
    ...overrides,
  };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
  await Promise.resolve();
  await Promise.resolve();
}

type ListenerBag = {
  position?: (e: Extract<PlayerEvent, { kind: "position" }>) => void;
  state?: (e: Extract<PlayerEvent, { kind: "state" }>) => void;
  tracks?: (e: Extract<PlayerEvent, { kind: "tracks" }>) => void;
  exit?: (e: Extract<PlayerEvent, { kind: "exit" }>) => void;
  error?: (e: Extract<PlayerEvent, { kind: "error" }>) => void;
};

function mountPlayer(
  host: HTMLElement,
  state: PlayerSessionState | null = makeSession(),
): { dispose: () => void; history: ReturnType<typeof createMemoryHistory> } {
  if (state) setPlayerSession(state);
  const history = createMemoryHistory();
  history.set({ value: "/player" });
  const dispose = render(
    () => (
      <MemoryRouter history={history}>
        <Route path="/player" component={Player} />
        <Route path="/" component={() => <div data-testid="home-stub">home</div>} />
        <Route
          path="/title/:id"
          component={() => <div data-testid="title-stub">title</div>}
        />
      </MemoryRouter>
    ),
    host,
  );
  return { dispose, history };
}

function wireEventMocks(): ListenerBag {
  const bag: ListenerBag = {};
  mockedOnPosition.mockImplementation((cb) => {
    bag.position = cb;
    return Promise.resolve(() => {});
  });
  mockedOnState.mockImplementation((cb) => {
    bag.state = cb;
    return Promise.resolve(() => {});
  });
  mockedOnTracks.mockImplementation((cb) => {
    bag.tracks = cb;
    return Promise.resolve(() => {});
  });
  mockedOnExit.mockImplementation((cb) => {
    bag.exit = cb;
    return Promise.resolve(() => {});
  });
  mockedOnError.mockImplementation((cb) => {
    bag.error = cb;
    return Promise.resolve(() => {});
  });
  return bag;
}

describe("Player route (F-015)", () => {
  let host: HTMLDivElement | null = null;
  let dispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    _resetPlayerSession();
    mockedPlayerOpen.mockReset().mockResolvedValue(undefined);
    mockedPlayerClose.mockReset().mockResolvedValue(true);
    mockedPlayerPause.mockReset().mockResolvedValue(undefined);
    mockedPlayerSeek.mockReset().mockResolvedValue(undefined);
    mockedPlayerSetAudio.mockReset().mockResolvedValue(undefined);
    mockedPlayerSetSub.mockReset().mockResolvedValue(undefined);
    mockedPlayerStatus.mockReset().mockResolvedValue(null);
    mockedBufferStart.mockReset().mockResolvedValue(undefined);
    mockedBufferStop.mockReset().mockResolvedValue(true);
    mockedStopPlayback.mockReset().mockResolvedValue(true);
    wireEventMocks();
  });

  afterEach(() => {
    dispose?.();
    host?.remove();
    host = null;
    dispose = null;
  });

  it("redirects to / when reached without a session", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host, null);
    dispose = mount.dispose;
    await flush();
    expect(mount.history.get()).toBe("/");
    expect(host.querySelector('[data-testid="home-stub"]')).not.toBeNull();
    expect(mockedPlayerOpen).not.toHaveBeenCalled();
  });

  it("calls playerOpen with the session payload on mount", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const session = makeSession();
    const mount = mountPlayer(host, session);
    dispose = mount.dispose;
    await flush();
    expect(mockedPlayerOpen).toHaveBeenCalledTimes(1);
    const arg = mockedPlayerOpen.mock.calls[0]![0]!;
    expect(arg.token).toBe(session.token);
    expect(arg.url).toBe(session.url);
    expect(arg.resumePositionS).toBe(0);
    expect(arg.cwContext).toEqual(session.cwContext);
  });

  it("starts the F-014 buffer monitor with the session token after open", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    expect(mockedBufferStart).toHaveBeenCalledTimes(1);
    expect(mockedBufferStart.mock.calls[0]![0]).toBe("session-token-uuid");
  });

  it("renders the display title in the header", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const title = host.querySelector('[data-testid="player-title"]');
    expect(title?.textContent).toBe("The Matrix");
  });

  it("reflects position events in the position label", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    bag.position?.({
      kind: "position",
      positionS: 125,
      durationS: 8000,
      paused: false,
    });
    await flush();
    const pos = host.querySelector('[data-testid="player-position"]');
    expect(pos?.textContent).toBe("2:05");
  });

  it("seek button dispatches playerSeek with a positive delta", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    bag.position?.({
      kind: "position",
      positionS: 100,
      durationS: 8000,
      paused: false,
    });
    await flush();
    const btn = host.querySelector<HTMLButtonElement>(
      '[data-testid="player-seek-forward"]',
    );
    expect(btn).not.toBeNull();
    btn!.click();
    await flush();
    expect(mockedPlayerSeek).toHaveBeenCalledTimes(1);
    expect(mockedPlayerSeek).toHaveBeenCalledWith(110);
  });

  it("seek-back button doesn't go below zero", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    bag.position?.({
      kind: "position",
      positionS: 3,
      durationS: 8000,
      paused: false,
    });
    await flush();
    const btn = host.querySelector<HTMLButtonElement>(
      '[data-testid="player-seek-back"]',
    );
    btn!.click();
    await flush();
    expect(mockedPlayerSeek).toHaveBeenCalledWith(0);
  });

  it("play/pause button toggles paused state via playerPause", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const btn = host.querySelector<HTMLButtonElement>(
      '[data-testid="player-play-pause"]',
    );
    expect(btn).not.toBeNull();
    btn!.click();
    await flush();
    expect(mockedPlayerPause).toHaveBeenCalledWith(true);
    btn!.click();
    await flush();
    expect(mockedPlayerPause).toHaveBeenLastCalledWith(false);
  });

  it("F-017 play-pause action also dispatches playerPause", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    emitAction("play-pause", "keyboard");
    await flush();
    expect(mockedPlayerPause).toHaveBeenCalledWith(true);
  });

  it("renders the audio track selector and dispatches playerSetAudioTrack", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const audio: AudioTrack[] = [
      {
        id: 1,
        title: null,
        language: "en",
        codec: "eac3",
        channels: 6,
        isDefault: true,
        isSelected: true,
      },
      {
        id: 2,
        title: "Director Commentary",
        language: "en",
        codec: "ac3",
        channels: 2,
        isDefault: false,
        isSelected: false,
      },
    ];
    bag.tracks?.({
      kind: "tracks",
      tracks: { audio, subtitles: [] },
    });
    await flush();
    const select = host.querySelector<HTMLSelectElement>(
      '[data-testid="player-audio-select"]',
    );
    expect(select).not.toBeNull();
    expect(select!.options.length).toBe(2);
    select!.value = "2";
    select!.dispatchEvent(new Event("change", { bubbles: true }));
    await flush();
    expect(mockedPlayerSetAudio).toHaveBeenCalledWith(2);
  });

  it("subtitle 'Off' choice maps to playerSetSubtitleTrack(null)", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const subs: SubtitleTrack[] = [
      {
        id: 3,
        title: null,
        language: "en",
        codec: "srt",
        isDefault: true,
        isForced: false,
        isSelected: true,
      },
    ];
    bag.tracks?.({
      kind: "tracks",
      tracks: { audio: [], subtitles: subs },
    });
    await flush();
    const select = host.querySelector<HTMLSelectElement>(
      '[data-testid="player-sub-select"]',
    );
    expect(select).not.toBeNull();
    select!.value = "";
    select!.dispatchEvent(new Event("change", { bubbles: true }));
    await flush();
    expect(mockedPlayerSetSub).toHaveBeenCalledWith(null);
  });

  it("seek bar input dispatches playerSeek using fraction × duration", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    bag.position?.({
      kind: "position",
      positionS: 0,
      durationS: 4000,
      paused: false,
    });
    await flush();
    const seek = host.querySelector<HTMLInputElement>(
      '[data-testid="player-seekbar"]',
    );
    expect(seek).not.toBeNull();
    seek!.value = "0.5";
    seek!.dispatchEvent(new Event("input", { bubbles: true }));
    await flush();
    expect(mockedPlayerSeek).toHaveBeenCalledWith(2000);
  });

  it("Exit event tears the session down and pops the route", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host, makeSession());
    dispose = mount.dispose;
    await flush();
    bag.exit?.({
      kind: "exit",
      positionS: 300,
      durationS: 8160,
      reachedEof: false,
    });
    await flush();
    expect(mockedPlayerClose).toHaveBeenCalled();
    expect(mockedBufferStop).toHaveBeenCalledWith("session-token-uuid");
    expect(mockedStopPlayback).toHaveBeenCalledWith("session-token-uuid", false);
  });

  it("Back button tears the session down", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const back = host.querySelector<HTMLButtonElement>(
      '[data-testid="player-back"]',
    );
    expect(back).not.toBeNull();
    back!.click();
    await flush();
    await flush();
    expect(mockedPlayerClose).toHaveBeenCalled();
    expect(mockedStopPlayback).toHaveBeenCalledWith("session-token-uuid", false);
  });

  it("F-017 back action also tears the session down", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    emitAction("back", "keyboard");
    await flush();
    await flush();
    expect(mockedPlayerClose).toHaveBeenCalled();
  });

  it("renders the error overlay when an Error event arrives", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const bag = wireEventMocks();
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    bag.error?.({ kind: "error", message: "socket closed" });
    await flush();
    const err = host.querySelector('[data-testid="player-error"]');
    expect(err?.textContent).toMatch(/socket closed/);
  });

  it("primes UI from playerStatus when the snapshot is non-null", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    const status: PlayerStatusResponse = {
      snapshot: {
        token: "session-token-uuid",
        state: "playing",
        positionS: 60,
        durationS: 4000,
        paused: false,
      },
      tracks: {
        audio: [
          {
            id: 1,
            title: null,
            language: "fr",
            codec: "aac",
            channels: 2,
            isDefault: true,
            isSelected: true,
          },
        ],
        subtitles: [],
      },
    };
    mockedPlayerStatus.mockResolvedValue(status);
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const pos = host.querySelector('[data-testid="player-position"]');
    expect(pos?.textContent).toBe("1:00");
    const audio = host.querySelector('[data-testid="player-audio-select"]');
    expect(audio).not.toBeNull();
  });

  it("paused state from playerStatus flips the play-pause label", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    mockedPlayerStatus.mockResolvedValue({
      snapshot: {
        token: "session-token-uuid",
        state: "paused",
        positionS: 0,
        durationS: 4000,
        paused: true,
      },
      tracks: { audio: [], subtitles: [] },
    });
    const mount = mountPlayer(host);
    dispose = mount.dispose;
    await flush();
    const btn = host.querySelector<HTMLButtonElement>(
      '[data-testid="player-play-pause"]',
    );
    expect(btn?.getAttribute("aria-pressed")).toBe("false");
  });
});

describe("formatTime", () => {
  it("formats sub-minute values with mm:ss", () => {
    expect(formatTime(42)).toBe("0:42");
  });

  it("formats minutes-only values with mm:ss", () => {
    expect(formatTime(2 * 60 + 5)).toBe("2:05");
  });

  it("formats hour values with h:mm:ss", () => {
    expect(formatTime(3 * 3600 + 12 * 60 + 7)).toBe("3:12:07");
  });

  it("collapses negative / NaN to 0:00", () => {
    expect(formatTime(-1)).toBe("0:00");
    expect(formatTime(Number.NaN)).toBe("0:00");
  });
});

describe("trackLabel", () => {
  it("prefers language + title when both are present", () => {
    const t: AudioTrack = {
      id: 1,
      title: "Original Audio",
      language: "en",
      codec: "eac3",
      channels: 6,
      isDefault: false,
      isSelected: false,
    };
    expect(trackLabel(t, 1)).toBe("EN Original Audio");
  });

  it("falls back to numeric Track label when nothing is known", () => {
    const t: AudioTrack = {
      id: 7,
      title: null,
      language: null,
      codec: null,
      channels: null,
      isDefault: false,
      isSelected: false,
    };
    expect(trackLabel(t, 7)).toMatch(/Track 7|Piste 7/);
  });

  it("appends the forced suffix for forced subtitles", () => {
    const t: SubtitleTrack = {
      id: 2,
      title: null,
      language: "en",
      codec: "srt",
      isDefault: false,
      isForced: true,
      isSelected: false,
    };
    expect(trackLabel(t, 2)).toMatch(/forced|forcés/);
  });
});
