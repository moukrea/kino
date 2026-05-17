// F-016 Settings route placeholder. F-016's session ships the full
// settings tree (API keys, Display, Language, Player, Cache, Network,
// Storage, About). The route exists so the F-008 nav rail's Settings
// entry has a destination.

import type { Component } from "solid-js";

import { t } from "../i18n";

export const Settings: Component = () => (
  <div class="flex h-full w-full flex-col gap-4 p-8">
    <h1 class="text-3xl font-bold text-neutral-50" data-testid="settings-title">
      {t("nav.settings")}
    </h1>
    <p class="text-neutral-400" data-testid="settings-coming-soon">
      {t("settings.comingSoon")}
    </p>
  </div>
);
