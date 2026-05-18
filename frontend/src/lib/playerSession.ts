// Module-level signal carrying the "pending playback session" between
// the originating route (PRD §F-010 TitleDetail Play / Resume) and the
// F-015 Player route. Routes that initiate playback call
// [`setPlayerSession`] with the playback handle + CW context, then
// navigate to `/player`; the Player route reads the signal on mount.
//
// We use a global signal instead of SolidJS Router navigation state
// because `createMemoryHistory` (used in vitest jsdom) silently drops
// the `state` payload — only the path is preserved. A global signal
// gives us a unit-testable handoff and matches the "one playback at a
// time" semantics the platform driver already enforces.

import { createSignal } from "solid-js";

import type { PlayerCwContext } from "./tauri";

/**
 * The payload the F-010 Title Detail route hands to the F-015 Player
 * route on navigation to `/player`. All fields are derived from the
 * [`startPlayback`] response + the CW row + the detail metadata.
 */
export type PlayerSessionState = {
  /** Token from `startPlayback`. The F-014 monitor + F-015 driver share it. */
  token: string;
  /** URL the platform driver consumes (local HTTP for torrents, raw for direct URLs). */
  url: string;
  /** PRD §F-012 resume position. 0 for fresh starts. */
  resumePositionS: number;
  /** Filename hint shown in the info panel. */
  fileName: string | null;
  /** Duration hint (seconds) for buffer-monitor math before mpv reports it. */
  durationHintS: number | null;
  /** CW context payload — backend writes every position tick / Exit. */
  cwContext: PlayerCwContext | null;
  /** Display title rendered in the info panel. */
  displayTitle: string;
};

const [pendingSession, setPendingSessionInternal] =
  createSignal<PlayerSessionState | null>(null);

/**
 * Set the active player session. The originating route MUST call this
 * before `navigate("/player")` so the Player route has data to boot
 * with — otherwise the Player route pops back to Home immediately.
 */
export function setPlayerSession(session: PlayerSessionState | null): void {
  setPendingSessionInternal(session);
}

/**
 * Read the active player session. Reactive — the Player route's
 * `onMount` can read once without subscribing. The session is NOT
 * cleared on read so the user can revisit the player route during
 * playback (e.g. background tab return) without losing context.
 */
export function getPlayerSession(): PlayerSessionState | null {
  return pendingSession();
}

/**
 * Reset the active session. Called by the Player route on tear-down
 * (Exit / Error / back navigation) so a subsequent `/player` navigation
 * without a fresh `setPlayerSession` pops back to Home instead of
 * reusing a stale handle.
 */
export function clearPlayerSession(): void {
  setPendingSessionInternal(null);
}

/**
 * Test-only reset. The Player tests reset the module signal between
 * cases to keep state leak-free.
 */
export function _resetForTests(): void {
  setPendingSessionInternal(null);
}
