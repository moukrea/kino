// App shell. PRD §F-008 establishes the nav-rail / route layout
// (Home, Movies, Series, Search, Settings); this module wires the
// SolidJS router on top of the F-017 input subsystem so D-pad / arrow
// / gamepad input still routes through the focus manager across route
// changes.
//
// The shell is intentionally thin: the nav rail lives on the left,
// the routed content on the right. Each route module (`routes/*.tsx`)
// owns its own layout. Initial focus inside the routed content is set
// by the route via `setInitialFocus` on mount (see `Home.tsx`).
//
// The Tauri host installs the database + addon machinery in `setup`;
// the frontend assumes commands are callable. When the bundle is run
// outside Tauri (vite dev / vitest jsdom) the `lib/tauri.ts` wrappers
// fall back to empty data so the UI still renders.

import {
  onCleanup,
  onMount,
  type Component,
  type JSX,
} from "solid-js";
import { Router, Route } from "@solidjs/router";

import { NavRail } from "./components/NavRail";
import { installInputSubsystem } from "./input";
import { Home } from "./routes/Home";
import { Movies } from "./routes/Movies";
import { Series } from "./routes/Series";
import { Search } from "./routes/Search";
import { Settings } from "./routes/Settings";

/**
 * Layout shared by every route: a fixed-width nav rail on the left,
 * the routed view to its right. Both stretch to the full viewport
 * height so the route can own its own scroll behavior.
 */
const Shell: Component<{ children?: JSX.Element }> = (props) => {
  onMount(() => {
    const uninstall = installInputSubsystem();
    onCleanup(uninstall);
  });

  return (
    <div
      class="flex h-screen w-screen bg-neutral-950 text-neutral-50"
      data-testid="app-shell"
    >
      <NavRail />
      <main class="relative flex-1 overflow-hidden">{props.children}</main>
    </div>
  );
};

const App: Component = () => (
  <Router root={Shell}>
    <Route path="/" component={Home} />
    <Route path="/movies" component={Movies} />
    <Route path="/series" component={Series} />
    <Route path="/search" component={Search} />
    <Route path="/settings" component={Settings} />
  </Router>
);

export default App;
