// PRD §F-015 native player overlay (Linux side).
//
// Architecture (per ADR-108): on Linux, the mpv driver runs as a
// subprocess that opens its own playback surface. This route is the
// Tauri-window "control surface" — a SolidJS overlay that:
//
//   - boots the platform driver via `playerOpen(...)` on mount,
//   - subscribes to the `player:*` Tauri events (position, state,
//     tracks, exit, error) and reflects them in the overlay UI,
//   - dispatches user input (play/pause, seek, audio/sub track select)
//     to the matching `player_*` Tauri command,
//   - kicks off the F-014 buffer monitor for the playback token so the
//     `<BufferOverlay>` renders the locked "Buffering for smooth
//     playback" UI on top of the controls when the engine is behind,
//   - tears the session down (`playerClose` + `stopPlayback`) and pops
//     back to the originating route on terminal events (Exit / Error /
//     user-pressed Back).
//
// The route does NOT compose its UI on top of a video surface — that's
// ADR-108's deferred half (the in-process libmpv-rs driver). The user
// sees mpv's own window for video and this route's window for controls;
// both close together on exit.
//
// Navigation contract: the TitleDetail stream-list click is the
// canonical entry point. It calls `startPlayback(source)`, then
// `setPlayerSession({ ... })` with the fields encoded in
// [`PlayerSessionState`], then `navigate("/player")`. Reaching
// `/player` without a pending session pops back to Home — the route
// is not directly addressable. See ADR-109 for why we use a
// module-level signal instead of Solid Router navigation state.

import {
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";
import { useNavigate } from "@solidjs/router";

import { BufferOverlay } from "../components/BufferOverlay";
import { Focusable } from "../components/Focusable";
import { setInitialFocus } from "../input/focus";
import { onAction } from "../input/keyboard";
import { t } from "../i18n";
import {
  clearPlayerSession,
  getPlayerSession,
  type PlayerSessionState,
} from "../lib/playerSession";
import {
  bufferStartMonitor,
  bufferStopMonitor,
  hasTauri,
  onPlayerError,
  onPlayerExit,
  onPlayerPosition,
  onPlayerState,
  onPlayerTracks,
  playerClose,
  playerOpen,
  playerPause,
  playerSeek,
  playerSetAudioTrack,
  playerSetSubtitleTrack,
  playerStatus,
  stopPlayback,
  type AudioTrack,
  type PlayerState,
  type PlayerTrackList,
  type SubtitleTrack,
} from "../lib/tauri";

export type { PlayerSessionState } from "../lib/playerSession";

/**
 * PRD §F-017: Space / A / Start collapse to the `play-pause` Action.
 * The route subscribes once at mount and routes the action into
 * `togglePause()`. Seek with arrow keys + buttons is a SEEK_STEP_S
 * skip in either direction.
 */
export const SEEK_STEP_S = 10;

/**
 * Format `seconds` as `H:MM:SS` (hour-elided when < 1h) for the seek
 * bar's time labels. Negative / non-finite inputs collapse to `0:00`.
 */
export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const total = Math.floor(seconds);
  const s = total % 60;
  const m = Math.floor(total / 60) % 60;
  const h = Math.floor(total / 3600);
  const ss = s.toString().padStart(2, "0");
  if (h > 0) {
    const mm = m.toString().padStart(2, "0");
    return `${h}:${mm}:${ss}`;
  }
  return `${m}:${ss}`;
}

/**
 * Human-friendly track label for the audio/subtitle dropdowns. Prefers
 * the language code + title; falls back to the numeric id so the user
 * still sees something pickable for unlabeled mpv tracks.
 */
export function trackLabel(
  track: AudioTrack | SubtitleTrack,
  fallbackId: number,
): string {
  const parts: string[] = [];
  if (track.language) parts.push(track.language.toUpperCase());
  if (track.title) parts.push(track.title);
  if (parts.length === 0) {
    parts.push(t("player.trackUntitled", { id: String(fallbackId) }));
  }
  if ("isForced" in track && track.isForced) {
    parts.push(t("player.trackForced"));
  }
  if (track.isDefault) {
    parts.push(t("player.trackDefault"));
  }
  return parts.join(" ");
}

export const Player: Component = () => {
  const navigate = useNavigate();
  // Read the session once on mount — the originating route sets it via
  // [`setPlayerSession`] just before navigating to `/player`. We cache
  // the read so re-renders don't see a `null` from `clearPlayerSession`
  // during tear-down.
  const sessionAtMount = getPlayerSession();
  const session = (): PlayerSessionState | null => sessionAtMount;

  // ---- player state mirrors ----
  const [playerState, setPlayerState] = createSignal<PlayerState>("loading");
  const [positionS, setPositionS] = createSignal<number>(0);
  const [durationS, setDurationS] = createSignal<number>(0);
  const [paused, setPaused] = createSignal<boolean>(false);
  const [tracks, setTracks] = createSignal<PlayerTrackList>({
    audio: [],
    subtitles: [],
  });
  const [errorMessage, setErrorMessage] = createSignal<string | null>(null);
  const [infoVisible, setInfoVisible] = createSignal<boolean>(false);

  const progressFraction = createMemo<number>(() => {
    const d = durationS();
    if (!Number.isFinite(d) || d <= 0) return 0;
    const p = Math.max(0, Math.min(positionS(), d));
    return p / d;
  });

  const playPauseLabel = createMemo<string>(() =>
    paused() ? t("player.play") : t("player.pause"),
  );

  // ---- backend lifecycle ----
  let bufferStarted = false;

  const teardown = async (): Promise<void> => {
    const s = session();
    clearPlayerSession();
    try {
      await playerClose();
    } catch (e) {
      console.warn("playerClose failed", e);
    }
    if (s) {
      if (bufferStarted) {
        try {
          await bufferStopMonitor(s.token);
        } catch (e) {
          console.warn("bufferStopMonitor failed", e);
        }
      }
      try {
        await stopPlayback(s.token, false);
      } catch (e) {
        console.warn("stopPlayback failed", e);
      }
    }
  };

  const goBack = (): void => {
    void teardown().finally(() => {
      navigate(-1);
    });
  };

  const togglePause = (): void => {
    const next = !paused();
    setPaused(next);
    void playerPause(next).catch((e) => {
      console.warn("playerPause failed", e);
    });
  };

  const seekBy = (deltaS: number): void => {
    const target = Math.max(0, positionS() + deltaS);
    setPositionS(target);
    void playerSeek(target).catch((e) => {
      console.warn("playerSeek failed", e);
    });
  };

  const seekTo = (target: number): void => {
    const clamped = Math.max(0, Math.min(target, durationS() || target));
    setPositionS(clamped);
    void playerSeek(clamped).catch((e) => {
      console.warn("playerSeek failed", e);
    });
  };

  const onSeekBarInput = (event: Event): void => {
    const target = event.currentTarget as HTMLInputElement;
    const fraction = Number(target.value);
    if (!Number.isFinite(fraction)) return;
    const d = durationS();
    if (d <= 0) return;
    seekTo(fraction * d);
  };

  const selectAudio = (trackId: number | null): void => {
    void playerSetAudioTrack(trackId).catch((e) => {
      console.warn("playerSetAudioTrack failed", e);
    });
  };

  const selectSubtitle = (trackId: number | null): void => {
    void playerSetSubtitleTrack(trackId).catch((e) => {
      console.warn("playerSetSubtitleTrack failed", e);
    });
  };

  // ---- mount: pop back if missing session, otherwise boot the driver ----
  onMount(() => {
    const s = session();
    if (!s) {
      // Direct navigation without state — bounce back to Home so the
      // route never sits in a "no stream" zombie state.
      navigate("/", { replace: true });
      return;
    }

    setPositionS(s.resumePositionS);
    if (s.durationHintS !== null) setDurationS(s.durationHintS);

    const unsubs: Array<() => void> = [];

    const wire = async (): Promise<void> => {
      if (!hasTauri()) return;

      // Subscribe BEFORE opening so we don't miss early state changes.
      unsubs.push(
        await onPlayerPosition((event) => {
          setPositionS(event.positionS);
          setDurationS(event.durationS);
          setPaused(event.paused);
        }),
      );
      unsubs.push(
        await onPlayerState((event) => {
          setPlayerState(event.state);
          if (event.state === "paused") setPaused(true);
          if (event.state === "playing") setPaused(false);
        }),
      );
      unsubs.push(
        await onPlayerTracks((event) => {
          setTracks(event.tracks);
        }),
      );
      unsubs.push(
        await onPlayerExit((event) => {
          setPositionS(event.positionS);
          setDurationS(event.durationS);
          setPlayerState(event.reachedEof ? "ended" : "idle");
          // Backend has already persisted CW via the bridge task; just
          // pop back so the user lands on the originating route.
          goBack();
        }),
      );
      unsubs.push(
        await onPlayerError((event) => {
          setErrorMessage(event.message);
          setPlayerState("error");
        }),
      );

      try {
        await playerOpen({
          token: s.token,
          url: s.url,
          resumePositionS: s.resumePositionS,
          fileName: s.fileName,
          durationHintS: s.durationHintS,
          cwContext: s.cwContext,
        });
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        setErrorMessage(message);
        setPlayerState("error");
        return;
      }

      // Kick off the F-014 buffer monitor so `<BufferOverlay>` reflects
      // SAFE / NEEDS_PREBUFFER / REBUFFER state on top of the controls.
      // `duration_s = 0` is the "unknown yet" sentinel the backend
      // accepts — once mpv emits a real `duration` property change the
      // monitor's published status flows the live value through.
      try {
        await bufferStartMonitor(s.token, s.durationHintS ?? 0);
        bufferStarted = true;
      } catch (e) {
        console.warn("bufferStartMonitor failed", e);
      }

      // Pull the first status so the overlay isn't stuck on
      // `loading` when player_open returned the snapshot already.
      try {
        const snap = await playerStatus();
        if (snap) {
          setPlayerState(snap.snapshot.state);
          setPositionS(snap.snapshot.positionS);
          setDurationS(snap.snapshot.durationS);
          setPaused(snap.snapshot.paused);
          setTracks(snap.tracks);
        }
      } catch (e) {
        console.warn("playerStatus failed", e);
      }
    };

    void wire();

    // Initial focus claims the play/pause button so D-pad users can
    // hit Enter / A immediately on arrival.
    queueMicrotask(() => {
      setInitialFocus("player-play-pause");
    });

    // F-017: Space (kbm) / A (gamepad) / Start (gamepad) → toggle pause.
    // Esc / B → back. The action handlers close over `paused()` /
    // signal setters; eslint-plugin-solid can't see across the
    // `onAction` boundary so the read is invisible to it. Suppression
    // matches the Focusable pattern.
    // eslint-disable-next-line solid/reactivity
    const unsubAction = onAction((action) => {
      if (action === "play-pause") {
        togglePause();
      } else if (action === "back") {
        goBack();
      }
    });

    onCleanup(() => {
      unsubAction();
      for (const u of unsubs) u();
      void teardown();
    });
  });

  return (
    <div
      class="relative flex h-full w-full flex-col bg-neutral-950 text-neutral-50"
      data-testid="player-root"
      role="region"
      aria-label={t("player.ariaLabelControls")}
    >
      <Show
        when={session()}
        fallback={
          <div
            class="flex h-full w-full items-center justify-center text-lg"
            data-testid="player-no-session"
          >
            {t("player.noStream")}
          </div>
        }
      >
        {(s) => (
          <div class="flex h-full w-full flex-col">
            {/* Header row: back + title + info toggle */}
            <header
              class="flex items-center gap-4 border-b border-neutral-800 bg-neutral-900/70 px-6 py-3"
              data-testid="player-header"
            >
              <Focusable id="player-back" onActivate={goBack}>
                {({ ref, showRing, onClick }) => (
                  <button
                    ref={ref as (el: HTMLButtonElement) => void}
                    type="button"
                    onClick={onClick}
                    data-testid="player-back"
                    class={`rounded bg-neutral-800/80 px-3 py-1 text-sm transition-transform ${
                      showRing()
                        ? "scale-[1.05] outline outline-2 outline-sky-400"
                        : ""
                    }`}
                  >
                    ← {t("player.back")}
                  </button>
                )}
              </Focusable>
              <div
                class="flex-1 truncate text-lg font-semibold"
                data-testid="player-title"
              >
                {s().displayTitle}
              </div>
              <Focusable
                id="player-info-toggle"
                onActivate={() => setInfoVisible(!infoVisible())}
              >
                {({ ref, showRing, onClick }) => (
                  <button
                    ref={ref as (el: HTMLButtonElement) => void}
                    type="button"
                    onClick={onClick}
                    data-testid="player-info-toggle"
                    class={`rounded bg-neutral-800/80 px-3 py-1 text-sm transition-transform ${
                      showRing()
                        ? "scale-[1.05] outline outline-2 outline-sky-400"
                        : ""
                    }`}
                  >
                    {infoVisible() ? t("player.infoHide") : t("player.info")}
                  </button>
                )}
              </Focusable>
            </header>

            {/* Body: status / info panel */}
            <section
              class="flex flex-1 flex-col items-center justify-center gap-6 px-6"
              data-testid="player-body"
            >
              <Show when={errorMessage()}>
                {(msg) => (
                  <div
                    class="rounded border border-red-700 bg-red-900/30 px-4 py-2 text-sm text-red-200"
                    data-testid="player-error"
                    role="alert"
                  >
                    {t("player.error", { message: msg() })}
                  </div>
                )}
              </Show>
              <Show when={!errorMessage()}>
                <div
                  class="text-3xl font-bold uppercase tracking-wider text-neutral-300"
                  data-testid="player-state"
                >
                  {playerState() === "loading" ||
                  playerState() === "buffering" ||
                  playerState() === "idle"
                    ? t("player.preparing")
                    : playerState() === "ended"
                      ? t("player.ended")
                      : paused()
                        ? t("player.pause")
                        : t("player.play")}
                </div>
              </Show>
              <Show when={infoVisible()}>
                <dl
                  class="grid grid-cols-2 gap-x-6 gap-y-1 rounded border border-neutral-800 bg-neutral-900/60 p-4 text-sm"
                  data-testid="player-info-panel"
                >
                  <dt class="text-neutral-400">File</dt>
                  <dd class="truncate">{s().fileName ?? "—"}</dd>
                  <dt class="text-neutral-400">URL</dt>
                  <dd class="truncate font-mono text-xs">{s().url}</dd>
                  <dt class="text-neutral-400">Token</dt>
                  <dd class="truncate font-mono text-xs">{s().token}</dd>
                  <dt class="text-neutral-400">State</dt>
                  <dd>{playerState()}</dd>
                </dl>
              </Show>
            </section>

            {/* Controls bar */}
            <footer
              class="flex flex-col gap-3 border-t border-neutral-800 bg-neutral-900/80 px-6 py-4"
              data-testid="player-controls"
            >
              <div class="flex items-center gap-4">
                <span
                  class="font-mono text-sm tabular-nums"
                  data-testid="player-position"
                >
                  {formatTime(positionS())}
                </span>
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.001"
                  value={progressFraction()}
                  onInput={onSeekBarInput}
                  data-testid="player-seekbar"
                  aria-label={t("player.ariaLabelSeekBar")}
                  aria-valuenow={progressFraction()}
                  class="flex-1 cursor-pointer accent-sky-400"
                  disabled={durationS() <= 0}
                />
                <span
                  class="font-mono text-sm tabular-nums"
                  data-testid="player-duration"
                >
                  {formatTime(durationS())}
                </span>
              </div>
              <div class="flex flex-wrap items-center gap-3">
                <Focusable
                  id="player-seek-back"
                  onActivate={() => seekBy(-SEEK_STEP_S)}
                >
                  {({ ref, showRing, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      type="button"
                      onClick={onClick}
                      data-testid="player-seek-back"
                      class={`rounded bg-neutral-800 px-3 py-2 text-sm transition-transform ${
                        showRing()
                          ? "scale-[1.05] outline outline-2 outline-sky-400"
                          : ""
                      }`}
                    >
                      ⏪ {t("player.seekBackward")}
                    </button>
                  )}
                </Focusable>
                <Focusable id="player-play-pause" onActivate={togglePause}>
                  {({ ref, showRing, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      type="button"
                      onClick={onClick}
                      data-testid="player-play-pause"
                      aria-pressed={!paused()}
                      class={`rounded bg-sky-500 px-4 py-2 text-sm font-semibold text-neutral-950 transition-transform ${
                        showRing()
                          ? "scale-[1.05] outline outline-2 outline-sky-300"
                          : ""
                      }`}
                    >
                      {paused() ? "▶" : "⏸"} {playPauseLabel()}
                    </button>
                  )}
                </Focusable>
                <Focusable
                  id="player-seek-forward"
                  onActivate={() => seekBy(SEEK_STEP_S)}
                >
                  {({ ref, showRing, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      type="button"
                      onClick={onClick}
                      data-testid="player-seek-forward"
                      class={`rounded bg-neutral-800 px-3 py-2 text-sm transition-transform ${
                        showRing()
                          ? "scale-[1.05] outline outline-2 outline-sky-400"
                          : ""
                      }`}
                    >
                      ⏩ {t("player.seekForward")}
                    </button>
                  )}
                </Focusable>

                {/* Audio track selector */}
                <Show when={tracks().audio.length > 0}>
                  <label
                    class="ml-4 flex items-center gap-2 text-sm"
                    data-testid="player-audio-label"
                  >
                    <span class="text-neutral-400">{t("player.audioTrack")}</span>
                    <select
                      data-testid="player-audio-select"
                      onChange={(e) => {
                        const v = e.currentTarget.value;
                        selectAudio(v === "" ? null : Number(v));
                      }}
                      class="rounded bg-neutral-800 px-2 py-1 text-sm"
                    >
                      <For each={tracks().audio}>
                        {(track) => (
                          <option
                            value={String(track.id)}
                            selected={track.isSelected}
                          >
                            {trackLabel(track, track.id)}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                </Show>

                {/* Subtitle track selector */}
                <Show when={tracks().subtitles.length > 0}>
                  <label
                    class="flex items-center gap-2 text-sm"
                    data-testid="player-sub-label"
                  >
                    <span class="text-neutral-400">
                      {t("player.subtitleTrack")}
                    </span>
                    <select
                      data-testid="player-sub-select"
                      onChange={(e) => {
                        const v = e.currentTarget.value;
                        selectSubtitle(v === "" ? null : Number(v));
                      }}
                      class="rounded bg-neutral-800 px-2 py-1 text-sm"
                    >
                      <option value="">{t("player.subtitleOff")}</option>
                      <For each={tracks().subtitles}>
                        {(track) => (
                          <option
                            value={String(track.id)}
                            selected={track.isSelected}
                          >
                            {trackLabel(track, track.id)}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                </Show>
              </div>
            </footer>

            {/* F-014 buffer overlay composited on top of the controls. */}
            <BufferOverlay token={s().token} />
          </div>
        )}
      </Show>
    </div>
  );
};

export default Player;
