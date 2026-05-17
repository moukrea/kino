// F-009 Series sub-home. Identical to Home, filtered to `kind="series"`.
// See `Movies.tsx` for the routing / mount story; the implementation is
// shared via `HomeView`.

import type { Component } from "solid-js";

import { HomeView } from "./Home";

export const Series: Component = () => <HomeView kind="series" />;
