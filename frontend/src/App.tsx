import type { Component } from "solid-js";

import { t } from "./i18n";

/**
 * Placeholder home screen required by PRD §F-001:
 * > "App launches and shows a placeholder home screen with the text 'kino'
 * >  on all targets."
 *
 * The real 10-foot UI (F-008) replaces this component end-to-end.
 */
const App: Component = () => {
  return (
    <main
      class="flex h-full w-full flex-col items-center justify-center gap-4 bg-neutral-950 text-neutral-50"
      role="main"
    >
      <h1 class="text-7xl font-bold tracking-tight" data-testid="home-title">
        {t("app.placeholderHome")}
      </h1>
      <p class="text-lg text-neutral-400" data-testid="home-tagline">
        {t("app.placeholderTagline")}
      </p>
    </main>
  );
};

export default App;
