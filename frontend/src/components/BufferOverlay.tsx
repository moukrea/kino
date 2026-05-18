// PRD §F-014 adaptive-buffer overlay.
//
// Renders the "Buffering for smooth playback" UI on top of the player
// surface whenever the backend's `buffer:status` event reports a state
// other than `safe`. The component is event-driven: it subscribes to
// `onBufferStatus(...)` on mount and unsubscribes on cleanup. The F-015
// player route mounts this component inside its full-screen player
// surface; routes that aren't a player don't need it.
//
// Progress math (PRD §F-014):
//
//   percent = clamp(piecesAheadSeconds / requiredPrebufferS, 0, 1)
//
// `requiredPrebufferS` only exists for `state === "needsPrebuffer"`. In
// the `rebuffer` branch the PRD shape is "pause until ahead is restored";
// we drive the bar off `piecesAheadSeconds / (SAFETY_MARGIN_S * 0.5)` so
// it visibly tracks recovery without needing a server-side hint.

import {
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";

import { t } from "../i18n";
import {
  type BufferStatusEvent,
  bufferStatus as fetchBufferStatus,
  onBufferStatus,
} from "../lib/tauri";

/**
 * PRD §8 `SAFETY_MARGIN_S = 30s`; the REBUFFER state recovers when
 * `piecesAheadSeconds >= SAFETY_MARGIN_S * 0.5`. Mirrored here so the
 * progress bar reaches 100% exactly when the backend's state machine
 * transitions back to SAFE.
 */
export const REBUFFER_RECOVERY_TARGET_S = 15.0;

export type BufferOverlayProps = {
  /** Token returned by `startPlayback`. The overlay filters incoming
   * events by token so multiple concurrent overlays (shouldn't happen,
   * but cheap to guard) don't cross-contaminate. */
  token: string;
};

const BYTES_PER_KB = 1024;
const BYTES_PER_MB = 1024 * 1024;

export function formatRate(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond < 0) return "0 B";
  if (bytesPerSecond >= BYTES_PER_MB) {
    return `${(bytesPerSecond / BYTES_PER_MB).toFixed(1)} MB`;
  }
  if (bytesPerSecond >= BYTES_PER_KB) {
    return `${(bytesPerSecond / BYTES_PER_KB).toFixed(0)} KB`;
  }
  return `${Math.round(bytesPerSecond)} B`;
}

export function formatEta(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds <= 0) {
    return t("buffer.etaUnknown");
  }
  if (seconds < 60) {
    return `${Math.round(seconds)}s`;
  }
  if (seconds < 3600) {
    const m = Math.floor(seconds / 60);
    const s = Math.round(seconds % 60);
    return `${m}m ${s.toString().padStart(2, "0")}s`;
  }
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return `${h}h ${m.toString().padStart(2, "0")}m`;
}

export function bufferProgress(status: BufferStatusEvent | null): number {
  if (!status) return 0;
  if (status.state === "safe") return 1;
  if (status.state === "rebuffer") {
    const target = REBUFFER_RECOVERY_TARGET_S;
    if (target <= 0) return 0;
    return Math.min(1, Math.max(0, status.piecesAheadSeconds / target));
  }
  const required = status.requiredPrebufferS ?? 0;
  if (required <= 0) return 0;
  return Math.min(1, Math.max(0, status.piecesAheadSeconds / required));
}

export const BufferOverlay: Component<BufferOverlayProps> = (props) => {
  const [status, setStatus] = createSignal<BufferStatusEvent | null>(null);

  onMount(() => {
    // Capture the mount-time token. The overlay is bound to a single
    // playback session for its lifetime; `props.token` does not change
    // (the player tears the component down between sessions). Holding
    // a local lets the async closures below close over the stable value
    // without tripping solid's reactivity lint.
    const token = props.token;
    let unlisten: (() => void) | null = null;
    void (async () => {
      // Pull the current snapshot first so the overlay shows correct
      // data on first paint without waiting for the next recompute.
      try {
        const initial = await fetchBufferStatus(token);
        if (initial && initial.token === token) setStatus(initial);
      } catch {
        // No monitor registered yet — that's fine, events will catch us up.
      }
      unlisten = await onBufferStatus((s) => {
        if (s.token === token) setStatus(s);
      });
    })();
    onCleanup(() => {
      if (unlisten) unlisten();
    });
  });

  const visible = createMemo(() => {
    const s = status();
    return s !== null && s.state !== "safe";
  });

  const progress = createMemo(() => bufferProgress(status()));
  const heading = createMemo(() => {
    const s = status();
    if (s?.state === "rebuffer") return t("buffer.rebuffering");
    return t("buffer.title");
  });

  return (
    <Show when={visible()}>
      <div
        class="buffer-overlay"
        role="status"
        aria-live="polite"
        aria-label={t("buffer.ariaLabel")}
      >
        <div class="buffer-overlay__card">
          <h2 class="buffer-overlay__title">{heading()}</h2>
          <div
            class="buffer-overlay__bar"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={1}
            aria-valuenow={progress()}
          >
            <div
              class="buffer-overlay__bar-fill"
              style={{ width: `${(progress() * 100).toFixed(1)}%` }}
            />
          </div>
          <div class="buffer-overlay__meta">
            <span class="buffer-overlay__rate">
              {t("buffer.downloadRate", {
                rate: t("buffer.ratePerSecond", {
                  n: formatRate(status()?.dlRateBytesPerS ?? 0),
                }),
              })}
            </span>
            <span class="buffer-overlay__eta">
              {t("buffer.eta", { eta: formatEta(status()?.etaSeconds ?? null) })}
            </span>
          </div>
        </div>
      </div>
    </Show>
  );
};

export default BufferOverlay;
