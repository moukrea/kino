// F-011 Search route placeholder. The route exists so the F-008 nav
// rail's Search entry navigates to a real page; the search bar /
// debounced live search / recent searches surface in F-011's session.

import type { Component } from "solid-js";

import { t } from "../i18n";

export const Search: Component = () => (
  <div class="flex h-full w-full flex-col gap-4 p-8">
    <h1 class="text-3xl font-bold text-neutral-50" data-testid="search-title">
      {t("nav.search")}
    </h1>
    <p class="text-neutral-400" data-testid="search-coming-soon">
      {t("search.comingSoon")}
    </p>
  </div>
);
