// Tests for the module-level player-session handoff used between
// TitleDetail (PRD §F-010) and the Player route (PRD §F-015).

import { afterEach, describe, expect, it } from "vitest";

import {
  _resetForTests,
  clearPlayerSession,
  getPlayerSession,
  setPlayerSession,
  type PlayerSessionState,
} from "./playerSession";

function dummySession(): PlayerSessionState {
  return {
    token: "tok-1",
    url: "http://127.0.0.1:9000/x",
    resumePositionS: 0,
    fileName: "movie.mkv",
    durationHintS: 7200,
    cwContext: null,
    displayTitle: "Movie",
  };
}

describe("playerSession", () => {
  afterEach(() => {
    _resetForTests();
  });

  it("starts with null", () => {
    expect(getPlayerSession()).toBeNull();
  });

  it("set / get roundtrip", () => {
    const s = dummySession();
    setPlayerSession(s);
    expect(getPlayerSession()).toEqual(s);
  });

  it("clearPlayerSession resets the signal", () => {
    setPlayerSession(dummySession());
    clearPlayerSession();
    expect(getPlayerSession()).toBeNull();
  });

  it("_resetForTests is equivalent to clearPlayerSession", () => {
    setPlayerSession(dummySession());
    _resetForTests();
    expect(getPlayerSession()).toBeNull();
  });
});
