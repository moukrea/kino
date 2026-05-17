// F-009 Movies sub-home. Identical to Home, filtered to `kind="movie"`:
// CW row hides if no movie entries, trending and weekly pools are the
// per-kind movie feeds from `kino-metadata`, and (once enumeration
// lands) addon catalogs are filtered to those whose manifest declares
// `"movie"` in its `types`.
//
// Route mounts the same `HomeView` component instance as `Home`, so
// switching between Home / Movies / Series via the nav rail re-uses the
// shared shell mount and is instant (PRD §F-009 acceptance).

import type { Component } from "solid-js";

import { HomeView } from "./Home";

export const Movies: Component = () => <HomeView kind="movie" />;
