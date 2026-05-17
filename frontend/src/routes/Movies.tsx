// F-009 Movies sub-home placeholder. The full filtered home (with the
// catalogs row and proper kind-filtered CW) lands in F-009's own
// session; for F-008 we mount `HomeView` with `kind="movie"` so the
// route exists, the nav-rail item works, and the trending pools come
// back filtered to movies.

import type { Component } from "solid-js";

import { HomeView } from "./Home";

export const Movies: Component = () => <HomeView kind="movie" />;
