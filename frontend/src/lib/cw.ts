// PRD §F-012 frontend helpers — formatting + badge selection for the
// Continue Watching row. The backend owns persistence + the locked
// next-episode rule (`cw_record_position`); this module owns the
// presentation choices that turn a CW row into a Tile badge label.

import { t } from "../i18n";
import type { ContinueWatching } from "./tauri";

/**
 * PRD §F-012 / §8 locked completion threshold. Mirrored on the frontend
 * so the Home CW row can decide between "Resume" and "Up next" labels
 * without a round-trip — the backend STILL applies the same threshold
 * authoritatively on every `cw_record_position` write.
 */
export const CW_COMPLETION_THRESHOLD = 0.95;

/**
 * PRD §F-012 series row label rules:
 *
 * - Movie row → no badge (the title alone is enough on the Home strip).
 * - Series row whose current episode has progress < 95% → "Resume Sxx Eyy".
 * - Series row whose current episode is ≥ 95% (i.e. the player has
 *   advanced this row to the next episode via the F-012 rule) → "Up
 *   next: Sxx Eyy".
 *
 * The frontend doesn't need to apply the next-episode rule itself: the
 * backend's `cw_record_position` already replaces a completed episode's
 * row with the next-episode row at progress 0. So a row at progress
 * exactly 0 with `(season, episode)` set is the "up next" state.
 */
export function cwTileBadge(cw: ContinueWatching): string | null {
  if (cw.kind === "movie") return null;
  if (cw.season <= 0 || cw.episode <= 0) return null;
  const params = {
    season: String(cw.season).padStart(2, "0"),
    episode: String(cw.episode).padStart(2, "0"),
  };
  const progress =
    cw.duration_s > 0 ? cw.position_s / cw.duration_s : 0;
  // PRD §F-012 "Up next" branch: the advanced row has position_s = 0
  // by construction (set by `cw_record_position`). Treat exactly-zero
  // (or near-zero, < 1s of accidental drift) as the up-next state so a
  // fresh row reads as "Up next: S01E02" instead of "Resume S01E02 at
  // 0s".
  if (cw.position_s < 1.0 && progress < CW_COMPLETION_THRESHOLD) {
    return t("home.cwUpNextEpisode", params);
  }
  return t("home.cwResumeEpisode", params);
}
