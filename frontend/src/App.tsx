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
  createEffect,
  createMemo,
  ErrorBoundary,
  onCleanup,
  onMount,
  type Component,
  type JSX,
} from "solid-js";
import { Router, Route, useNavigate, useLocation } from "@solidjs/router";

import { NavRail } from "./components/NavRail";
import { setLocale, t, type SupportedLocale, SUPPORTED_LOCALES } from "./i18n";
import { installInputSubsystem, onAction } from "./input";
import {
  setOverride as setInputOverride,
  type InputProfileOverride,
} from "./input/profile";
import { setShowUnavailable } from "./lib/displaySettings";
import { hasTauri, settingsGetAll } from "./lib/tauri";
import { Home } from "./routes/Home";
import { Movies } from "./routes/Movies";
import { Series } from "./routes/Series";
import { Search, SEARCH_INPUT_TEST_ID } from "./routes/Search";
import { Settings } from "./routes/Settings";
import { TitleDetail } from "./routes/TitleDetail";
import { Player } from "./routes/Player";

const INPUT_OVERRIDE_VALUES: readonly InputProfileOverride[] = [
  "auto",
  "touch",
  "dpad",
  "kbm",
];

/**
 * Layout shared by every route: a fixed-width nav rail on the left,
 * the routed view to its right. Both stretch to the full viewport
 * height so the route can own its own scroll behavior.
 */
const Shell: Component<{ children?: JSX.Element }> = (props) => {
  const navigate = useNavigate();
  const location = useLocation();

  onMount(() => {
    const uninstall = installInputSubsystem();
    // PRD §F-016: load persisted UI language + input-profile override on
    // boot so the user's choices survive restarts ("All settings persist
    // across restarts"). Failure here is silent — the default locale
    // detector + "auto" profile keep the app usable.
    if (hasTauri()) {
      void settingsGetAll()
        .then((view) => {
          const uiLang = view.language.ui;
          if (
            uiLang &&
            (SUPPORTED_LOCALES as readonly string[]).includes(uiLang)
          ) {
            setLocale(uiLang as SupportedLocale);
          }
          const override = view.display.input_override;
          if (
            override &&
            (INPUT_OVERRIDE_VALUES as readonly string[]).includes(override)
          ) {
            setInputOverride(override as InputProfileOverride);
          }
          // PRD §F-006 / §F-016: hydrate the live "show unavailable"
          // toggle so catalog rows mounted before Settings has been
          // visited honor the persisted choice immediately.
          setShowUnavailable(view.display.show_unavailable);
        })
        .catch(() => {
          // First-boot DB may not be ready yet; keep going with defaults.
        });
    }
    // PRD §F-011: `/` on keyboard and Y on gamepad focus search "from
    // anywhere". Both inputs collapse to the `search` Action at the
    // F-017 layer; the shell listens once and routes to /search
    // (focusing the input is the route's `onMount` responsibility).
    const unsubscribe = onAction((action) => {
      if (action !== "search") return;
      if (location.pathname !== "/search") {
        navigate("/search");
        return;
      }
      // Already on /search — re-focus the input via its test-id so the
      // shortcut still snaps focus back if the user has navigated to a
      // result tile.
      const el = document.querySelector<HTMLInputElement>(
        `[data-testid="${SEARCH_INPUT_TEST_ID}"]`,
      );
      el?.focus();
    });
    onCleanup(() => {
      uninstall();
      unsubscribe();
    });
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

/**
 * Root-level fallback rendered by [`App`]'s top-level
 * [`ErrorBoundary`]. PRD §5 Reliability locks "Frontend errors caught at
 * root error boundary and logged"; the boundary catches anything that
 * escapes the router (route loaders, render errors, signal handlers
 * during render) and the fallback surfaces a retry surface so the user
 * isn't stuck on a blank screen. The error itself is logged via
 * `console.error` which the Tauri webview relays into the
 * `tracing`-backed file appender installed by the host.
 *
 * Exported for unit testing — production code only mounts this through
 * the [`App`]-level boundary.
 */
export const RootErrorFallback: Component<{
  error: unknown;
  reset: () => void;
}> = (props) => {
  const message = createMemo(() => {
    const e = props.error as { message?: unknown } | null;
    if (e && typeof e.message === "string") return e.message;
    try {
      return String(props.error);
    } catch {
      return "unknown error";
    }
  });
  // Log on every error change so a re-throw after reset is captured too.
  // PRD §5: "Frontend errors caught at root error boundary and logged".
  // The Tauri webview relays `console.error` into the host's tracing
  // file appender via stderr.
  createEffect(() => {
    console.error("kino: root error boundary caught", props.error);
  });
  return (
    <div
      class="flex h-screen w-screen flex-col items-center justify-center gap-4 bg-neutral-950 p-8 text-neutral-50"
      data-testid="app-error-boundary"
      role="alert"
    >
      <h1 class="text-2xl font-bold">{t("app.errorTitle")}</h1>
      <p class="max-w-xl text-center text-neutral-300">
        {t("app.errorBody")}
      </p>
      <pre
        class="max-w-2xl overflow-auto rounded bg-neutral-900 p-4 text-left text-xs text-neutral-400"
        data-testid="app-error-message"
      >
        {message()}
      </pre>
      <button
        type="button"
        class="rounded bg-neutral-200 px-4 py-2 text-sm font-medium text-neutral-900 hover:bg-neutral-50"
        data-testid="app-error-retry"
        onClick={() => props.reset()}
      >
        {t("app.errorRetry")}
      </button>
    </div>
  );
};

const App: Component = () => (
  <ErrorBoundary
    fallback={(err, reset) => (
      <RootErrorFallback error={err} reset={reset} />
    )}
  >
    <Router root={Shell}>
      <Route path="/" component={Home} />
      <Route path="/movies" component={Movies} />
      <Route path="/series" component={Series} />
      <Route path="/search" component={Search} />
      <Route path="/settings" component={Settings} />
      <Route path="/title/:id" component={TitleDetail} />
      <Route path="/player" component={Player} />
    </Router>
  </ErrorBoundary>
);

export default App;
