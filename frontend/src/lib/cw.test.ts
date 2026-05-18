// PRD §F-012 frontend badge resolution. The backend owns persistence
// and the locked next-episode rule; this module owns the presentation
// choice between "Resume Sxx Eyy" and "Up next: Sxx Eyy" badges on the
// Home Continue Watching row tiles.

import { describe, expect, it } from "vitest";

import { cwTileBadge, CW_COMPLETION_THRESHOLD } from "./cw";
import type { ContinueWatching } from "./tauri";

function row(over: Partial<ContinueWatching>): ContinueWatching {
  return {
    title_id: "tt0944947",
    kind: "series",
    season: 1,
    episode: 1,
    position_s: 600,
    duration_s: 1800,
    last_played_at: 0,
    meta_json: {},
    ...over,
  };
}

describe("cwTileBadge", () => {
  it("returns null for movie rows so the title stands alone", () => {
    const cw = row({ kind: "movie", season: 0, episode: 0 });
    expect(cwTileBadge(cw)).toBeNull();
  });

  it("returns Resume label for a series row with progress < 95%", () => {
    const cw = row({ position_s: 900, duration_s: 1800 }); // 50%
    const label = cwTileBadge(cw);
    expect(label).not.toBeNull();
    expect(label).toContain("Resume");
    expect(label).toContain("S01");
    expect(label).toContain("E01");
  });

  it("returns Up next label for an advanced row (position 0)", () => {
    // PRD §F-012 advanced row: position_s = 0 by construction (set
    // by `cw_record_position`). The badge surfaces "Up next" so the
    // user sees the next episode is queued.
    const cw = row({
      season: 1,
      episode: 5,
      position_s: 0,
      duration_s: 0,
    });
    const label = cwTileBadge(cw);
    expect(label).not.toBeNull();
    expect(label).toContain("Up next");
    expect(label).toContain("S01");
    expect(label).toContain("E05");
  });

  it("returns null for series rows missing a season/episode tag", () => {
    // Defensive: a corrupt CW row with season=0 / episode=0 on a
    // series shouldn't crash; render as no-badge so the tile still
    // displays.
    const cw = row({ season: 0, episode: 0 });
    expect(cwTileBadge(cw)).toBeNull();
  });

  it("zero-pads single-digit season/episode numbers", () => {
    const cw = row({ season: 3, episode: 9, position_s: 100, duration_s: 1800 });
    const label = cwTileBadge(cw);
    expect(label).toContain("S03");
    expect(label).toContain("E09");
  });

  it("treats a row exactly at the completion threshold as Resume (rule applies at write, not read)", () => {
    // The backend has already applied the next-episode rule on
    // write. If a row survives at the threshold it's because the
    // sweep hasn't run yet — render it as Resume rather than
    // Up next.
    const cw = row({
      position_s: CW_COMPLETION_THRESHOLD * 1800,
      duration_s: 1800,
    });
    const label = cwTileBadge(cw);
    expect(label).toContain("Resume");
  });
});
