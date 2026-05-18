// F-016 Settings screen (PRD §F-016 sections 1-8).
//
// One route, eight sections, all D-pad navigable (PRD §F-016 §6A code
// acceptance: "All settings navigable end-to-end with D-pad only"). Every
// interactive control is wrapped in a Focusable so the F-017 geometric
// focus manager can traverse the form without `tab` mediation.
//
// Persistence path:
//   - `settingsGetAll()` populates initial state.
//   - Each control writes back via `settingsSet(key, value)`. The backend
//     normalizes + validates and returns the canonical string; the route
//     mirrors that into local state.
//   - Save status is surfaced via a small banner that auto-clears after
//     2 seconds (success) or stays until the next attempt (failure).
//   - Reset to defaults goes through the confirmation modal then calls
//     `settingsResetDefaults()` and reloads the view.

import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  Index,
  Match,
  on,
  onMount,
  Show,
  Switch,
  untrack,
  type Accessor,
  type Component,
  type JSX,
} from "solid-js";

import { Focusable } from "../components/Focusable";
import { setInitialFocus } from "../input/focus";
import {
  detectPlatform,
  override as inputOverride,
  setOverride as setInputOverride,
  type InputProfileOverride,
} from "../input/profile";
import { locale, setLocale, type SupportedLocale, t, tDyn } from "../i18n";
import {
  addonsList,
  addonsSetEnabled,
  cacheClear,
  cacheUsageBytes,
  exportLogs,
  getAppInfo,
  getRecommendedAddons,
  hasTauri,
  installAddon,
  setAddonOrder,
  settingsGetAll,
  settingsResetDefaults,
  settingsSet,
  SETTING_KEYS,
  testFanart,
  testTmdb,
  testTrakt,
  testTvdb,
  uninstallAddon,
  type AddonRow,
  type AppInfo,
  type RecommendedAddon,
  type SettingsView,
} from "../lib/tauri";

/** Stable id for the first interactive control on the Settings route. */
export const SETTINGS_INITIAL_FOCUS_ID = "settings-section-apiKeys-tmdb-input";

/** Status banner messages auto-clear after this many ms on success. */
const STATUS_AUTO_CLEAR_MS = 2000;

type SaveStatus = { kind: "ok" | "error"; message: string } | null;

type ApiTestKind = "tmdb" | "trakt" | "tvdb" | "fanart";
type ApiTestStatus = "idle" | "testing" | "ok" | { error: string };

type SettingsLoader = () => Promise<{
  view: SettingsView;
  addons: AddonRow[];
  recommended: RecommendedAddon[];
  appInfo: AppInfo;
}>;

const DEFAULT_VIEW: SettingsView = {
  api_keys: { tmdb: "", trakt: "", tvdb: "", fanart: "" },
  language: { metadata_primary: "", metadata_fallback: [], ui: "" },
  cache: { path: "", size_gib: 4, min_gib: 1, max_gib: 100 },
  buffer: {
    safety_margin_s: 30,
    prebuffer_target_s: 15,
    piece_high_s: 60,
    piece_med_s: 300,
    recompute_interval_s: 5,
  },
  player: {
    passthrough_truehd: true,
    passthrough_dtshd: true,
    passthrough_dtsx: true,
    passthrough_atmos: true,
    passthrough_ac3: true,
    passthrough_dts: true,
    passthrough_eac3: true,
    force_hw_decode: true,
    tunneling: true,
  },
  display: {
    tile_size: "medium",
    focus_animation: true,
    nsfw: false,
    input_override: "auto",
    high_contrast: false,
  },
};

const DEFAULT_APP_INFO: AppInfo = {
  name: "kino",
  version: "1.0.0-alpha.1",
  commit: "unknown",
  repository: "",
  license: "MIT",
  platform: "linux",
};

const defaultLoader: SettingsLoader = async () => {
  if (!hasTauri()) {
    return {
      view: DEFAULT_VIEW,
      addons: [],
      recommended: [],
      appInfo: DEFAULT_APP_INFO,
    };
  }
  const [view, addons, recommended, appInfo] = await Promise.all([
    settingsGetAll(),
    addonsList(),
    getRecommendedAddons(),
    getAppInfo(),
  ]);
  return { view, addons, recommended, appInfo };
};

export type SettingsProps = {
  /**
   * Test seam so vitest can inject a synchronous loader without standing
   * up the full Tauri IPC stack. Production wires the live commands.
   */
  loader?: SettingsLoader;
};

/** Route entry point used by `App.tsx`. Wraps [`SettingsView`] with the
 * default Tauri-backed loader so the SolidJS router contract
 * (`Component<RouteSectionProps>`) matches. */
export const Settings: Component = () => <SettingsContent />;

/** Inner component carrying the optional `loader` test seam. Exported for
 * vitest only. */
export const SettingsContent: Component<SettingsProps> = (props) => {
  // Capture `props.loader` reactively so SolidJS notices when the test seam
  // changes the loader mid-render (vitest does this between cases). The
  // explicit memo keeps `eslint-plugin-solid` happy about the prop read.
  const loaderAccessor = createMemo(() => props.loader ?? defaultLoader);
  const [data, { refetch }] = createResource(() => loaderAccessor()());
  const [status, setStatus] = createSignal<SaveStatus>(null);
  const [pendingConfirm, setPendingConfirm] = createSignal<{
    message: string;
    onConfirm: () => void | Promise<void>;
  } | null>(null);

  onMount(() => {
    setInitialFocus(SETTINGS_INITIAL_FOCUS_ID);
  });

  function announce(next: SaveStatus): void {
    setStatus(next);
    if (next?.kind === "ok") {
      setTimeout(() => {
        setStatus((cur) => (cur === next ? null : cur));
      }, STATUS_AUTO_CLEAR_MS);
    }
  }

  async function persist(key: string, value: string): Promise<string | null> {
    try {
      const normalized = hasTauri() ? await settingsSet(key, value) : value;
      announce({ kind: "ok", message: t("settings.saved") });
      return normalized;
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      announce({
        kind: "error",
        message: t("settings.saveFailed", { reason: message }),
      });
      return null;
    }
  }

  function requestConfirm(message: string, onConfirm: () => void | Promise<void>) {
    setPendingConfirm({ message, onConfirm });
  }

  async function runReset(): Promise<void> {
    try {
      if (hasTauri()) {
        await settingsResetDefaults();
      }
      await refetch();
      announce({ kind: "ok", message: t("settings.resetSuccess") });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      announce({
        kind: "error",
        message: t("settings.saveFailed", { reason: message }),
      });
    }
  }

  const view = createMemo<SettingsView>(() => data()?.view ?? DEFAULT_VIEW);
  const addons = createMemo<AddonRow[]>(() => data()?.addons ?? []);
  const recommended = createMemo<RecommendedAddon[]>(
    () => data()?.recommended ?? [],
  );
  const appInfo = createMemo<AppInfo>(() => data()?.appInfo ?? DEFAULT_APP_INFO);
  const isAndroid = createMemo(() => appInfo().platform === "android");

  return (
    <div
      class="flex h-full w-full flex-col gap-4 overflow-y-auto p-8"
      data-testid="settings-root"
    >
      <header class="flex flex-wrap items-center justify-between gap-4">
        <h1 class="text-3xl font-bold text-neutral-50" data-testid="settings-title">
          {t("settings.title")}
        </h1>
        <Focusable
          id="settings-reset"
          onActivate={() =>
            requestConfirm(t("settings.resetConfirm"), runReset)
          }
        >
          {({ showRing, ref, onClick }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              onClick={onClick}
              data-testid="settings-reset"
              class={`rounded-md bg-red-600 px-4 py-2 text-sm font-semibold text-neutral-50 hover:bg-red-500 ${
                showRing() ? "outline outline-2 outline-sky-400" : ""
              }`}
            >
              {t("settings.resetDefaults")}
            </button>
          )}
        </Focusable>
      </header>

      <Show when={status()}>
        {(banner) => (
          <div
            data-testid="settings-status"
            data-status-kind={banner().kind}
            class={`rounded-md px-4 py-2 text-sm ${
              banner().kind === "ok"
                ? "bg-emerald-900/40 text-emerald-200"
                : "bg-red-900/40 text-red-200"
            }`}
          >
            {banner().message}
          </div>
        )}
      </Show>

      <Show
        when={!data.loading}
        fallback={
          <p class="text-neutral-400" data-testid="settings-loading">
            {t("settings.loading")}
          </p>
        }
      >
        <div class="flex flex-col gap-6">
          <ApiKeysSection view={view} persist={persist} announce={announce} />
          <AddonsSection
            addons={addons}
            recommended={recommended}
            refetch={() => {
              void refetch();
            }}
            announce={announce}
            requestConfirm={requestConfirm}
          />
          <LanguageSection view={view} persist={persist} />
          <CacheSection view={view} persist={persist} requestConfirm={requestConfirm} />
          <BufferSection view={view} persist={persist} />
          <Show when={isAndroid()}>
            <PlayerSection view={view} persist={persist} />
          </Show>
          <DisplaySection view={view} persist={persist} />
          <AboutSection appInfo={appInfo} announce={announce} />
        </div>
      </Show>

      <Show when={pendingConfirm()}>
        {(modal) => (
          <ConfirmModal
            message={modal().message}
            onConfirm={() => {
              const fn = modal().onConfirm;
              setPendingConfirm(null);
              void fn();
            }}
            onCancel={() => setPendingConfirm(null)}
          />
        )}
      </Show>
    </div>
  );
};

// ---- Section components --------------------------------------------------

type PersistFn = (key: string, value: string) => Promise<string | null>;
type AnnounceFn = (next: SaveStatus) => void;

type SectionProps = {
  view: Accessor<SettingsView>;
  persist: PersistFn;
};

const SectionShell: Component<{
  id: string;
  titleKey: string;
  children: JSX.Element;
}> = (props) => (
  <section
    class="rounded-lg border border-neutral-800 bg-neutral-900/50 p-6"
    data-testid={`settings-section-${props.id}`}
  >
    <h2 class="mb-4 text-xl font-semibold text-neutral-100">
      {tDyn(props.titleKey)}
    </h2>
    <div class="flex flex-col gap-4">{props.children}</div>
  </section>
);

const ApiKeysSection: Component<
  SectionProps & { announce: AnnounceFn }
> = (props) => (
  <SectionShell id="apiKeys" titleKey="settings.sections.apiKeys">
    <ApiKeyRow
      idPrefix="settings-section-apiKeys-tmdb"
      labelKey="settings.apiKeys.tmdb"
      hintKey="settings.apiKeys.tmdbRequired"
      linkKey="settings.apiKeys.tmdbLink"
      href="https://www.themoviedb.org/settings/api"
      value={() => props.view().api_keys.tmdb}
      kvKey={SETTING_KEYS.apiTmdb}
      persist={props.persist}
      testKind="tmdb"
    />
    <ApiKeyRow
      idPrefix="settings-section-apiKeys-trakt"
      labelKey="settings.apiKeys.trakt"
      linkKey="settings.apiKeys.traktLink"
      href="https://trakt.tv/oauth/applications"
      value={() => props.view().api_keys.trakt}
      kvKey={SETTING_KEYS.apiTrakt}
      persist={props.persist}
      testKind="trakt"
    />
    <ApiKeyRow
      idPrefix="settings-section-apiKeys-tvdb"
      labelKey="settings.apiKeys.tvdb"
      linkKey="settings.apiKeys.tvdbLink"
      href="https://thetvdb.com/api-information"
      value={() => props.view().api_keys.tvdb}
      kvKey={SETTING_KEYS.apiTvdb}
      persist={props.persist}
      testKind="tvdb"
    />
    <ApiKeyRow
      idPrefix="settings-section-apiKeys-fanart"
      labelKey="settings.apiKeys.fanart"
      linkKey="settings.apiKeys.fanartLink"
      href="https://fanart.tv/get-an-api-key"
      value={() => props.view().api_keys.fanart}
      kvKey={SETTING_KEYS.apiFanart}
      persist={props.persist}
      testKind="fanart"
    />
  </SectionShell>
);

const TEST_FNS: Record<ApiTestKind, () => Promise<void>> = {
  tmdb: testTmdb,
  trakt: testTrakt,
  tvdb: testTvdb,
  fanart: testFanart,
};

const ApiKeyRow: Component<{
  idPrefix: string;
  labelKey: string;
  hintKey?: string;
  linkKey: string;
  href: string;
  value: Accessor<string>;
  kvKey: string;
  persist: PersistFn;
  testKind: ApiTestKind;
}> = (props) => {
  const [draft, setDraft] = createSignal(untrack(() => props.value()));
  const [testStatus, setTestStatus] = createSignal<ApiTestStatus>("idle");

  // Re-sync the draft when the upstream value changes (e.g. after reset).
  createEffect(on(() => props.value(), (next) => setDraft(next)));

  async function runTest(): Promise<void> {
    setTestStatus("testing");
    try {
      if (hasTauri()) {
        await TEST_FNS[props.testKind]();
      }
      setTestStatus("ok");
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      setTestStatus({ error: message });
    }
  }

  return (
    <div class="flex flex-col gap-2">
      <label class="text-sm font-semibold text-neutral-200">
        {tDyn(props.labelKey)}
      </label>
      <Show when={props.hintKey}>
        <p class="text-xs text-neutral-400">{tDyn(props.hintKey!)}</p>
      </Show>
      <div class="flex flex-wrap items-center gap-2">
        <Focusable id={`${props.idPrefix}-input`}>
          {({ showRing, ref }) => (
            <input
              ref={ref as (el: HTMLInputElement) => void}
              type="password"
              value={draft()}
              onInput={(e) => setDraft(e.currentTarget.value)}
              onChange={async (e) => {
                const next = e.currentTarget.value;
                const saved = await props.persist(props.kvKey, next);
                if (saved !== null) setDraft(saved);
              }}
              data-testid={`${props.idPrefix}-input`}
              placeholder={t("settings.apiKeys.placeholderUnset")}
              class={`flex-1 rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
                showRing() ? "outline outline-2 outline-sky-400" : ""
              }`}
            />
          )}
        </Focusable>
        <Focusable id={`${props.idPrefix}-test`} onActivate={runTest}>
          {({ showRing, ref, onClick }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              onClick={onClick}
              data-testid={`${props.idPrefix}-test`}
              class={`rounded-md bg-sky-600 px-3 py-2 text-sm font-semibold text-neutral-50 hover:bg-sky-500 ${
                showRing() ? "outline outline-2 outline-sky-400" : ""
              }`}
            >
              {testStatus() === "testing"
                ? t("settings.apiKeys.testing")
                : t("settings.apiKeys.test")}
            </button>
          )}
        </Focusable>
      </div>
      <Switch>
        <Match when={testStatus() === "ok"}>
          <p
            class="text-xs text-emerald-300"
            data-testid={`${props.idPrefix}-test-result`}
          >
            {t("settings.apiKeys.testOk")}
          </p>
        </Match>
        <Match when={typeof testStatus() === "object"}>
          <p
            class="text-xs text-red-300"
            data-testid={`${props.idPrefix}-test-result`}
          >
            {t("settings.apiKeys.testFailed", {
              reason: (testStatus() as { error: string }).error,
            })}
          </p>
        </Match>
      </Switch>
      <Focusable id={`${props.idPrefix}-link`}>
        {({ showRing, ref }) => (
          <a
            ref={ref as (el: HTMLAnchorElement) => void}
            href={props.href}
            target="_blank"
            rel="noreferrer"
            data-testid={`${props.idPrefix}-link`}
            class={`text-xs text-sky-300 underline ${
              showRing() ? "outline outline-2 outline-sky-400" : ""
            }`}
          >
            {tDyn(props.linkKey)} ↗
          </a>
        )}
      </Focusable>
    </div>
  );
};

const AddonsSection: Component<{
  addons: Accessor<AddonRow[]>;
  recommended: Accessor<RecommendedAddon[]>;
  refetch: () => void;
  announce: AnnounceFn;
  requestConfirm: (message: string, fn: () => Promise<void>) => void;
}> = (props) => {
  const [addUrl, setAddUrl] = createSignal("");
  const [installing, setInstalling] = createSignal(false);

  async function addByUrl(): Promise<void> {
    const url = addUrl().trim();
    if (!url) return;
    setInstalling(true);
    try {
      if (hasTauri()) {
        await installAddon(url);
      }
      setAddUrl("");
      props.refetch();
      props.announce({ kind: "ok", message: t("settings.saved") });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.addons.installError", { reason: message }),
      });
    } finally {
      setInstalling(false);
    }
  }

  async function setEnabled(id: string, enabled: boolean): Promise<void> {
    try {
      if (hasTauri()) {
        await addonsSetEnabled(id, enabled);
      }
      props.refetch();
      props.announce({ kind: "ok", message: t("settings.saved") });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.saveFailed", { reason: message }),
      });
    }
  }

  async function move(id: string, direction: "up" | "down"): Promise<void> {
    const list = props.addons();
    const idx = list.findIndex((a) => a.id === id);
    if (idx === -1) return;
    const target = direction === "up" ? idx - 1 : idx + 1;
    if (target < 0 || target >= list.length) return;
    try {
      if (hasTauri()) {
        await setAddonOrder(id, target);
      }
      props.refetch();
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.saveFailed", { reason: message }),
      });
    }
  }

  async function uninstall(addon: AddonRow): Promise<void> {
    try {
      if (hasTauri()) {
        await uninstallAddon(addon.id);
      }
      props.refetch();
      props.announce({ kind: "ok", message: t("settings.saved") });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.saveFailed", { reason: message }),
      });
    }
  }

  async function installRecommended(rec: RecommendedAddon): Promise<void> {
    try {
      if (hasTauri()) {
        await installAddon(rec.manifest_url);
      }
      props.refetch();
      props.announce({ kind: "ok", message: t("settings.saved") });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.addons.installError", { reason: message }),
      });
    }
  }

  function addonName(addon: AddonRow): string {
    const manifest = addon.manifest_json as { name?: string } | null;
    return manifest?.name ?? addon.id;
  }

  function addonTypes(addon: AddonRow): string {
    const manifest = addon.manifest_json as { types?: string[] } | null;
    return (manifest?.types ?? []).join(", ");
  }

  function isCinemeta(addon: AddonRow): boolean {
    return addon.manifest_url === "https://v3-cinemeta.strem.io/manifest.json";
  }

  return (
    <SectionShell id="addons" titleKey="settings.sections.addons">
      <h3 class="text-sm font-semibold text-neutral-300">
        {t("settings.addons.installed")}
      </h3>
      <Show
        when={props.addons().length > 0}
        fallback={
          <p class="text-sm text-neutral-400">
            {t("settings.addons.noneInstalled")}
          </p>
        }
      >
        <ul class="flex flex-col gap-2" data-testid="settings-addons-list">
          <Index each={props.addons()}>
            {(addon, idx) => (
              <li
                class="flex flex-wrap items-center gap-2 rounded-md border border-neutral-800 bg-neutral-950 p-3"
                data-testid={`settings-addon-${addon().id}`}
                data-order={idx}
              >
                <div class="flex-1">
                  <p class="font-semibold text-neutral-100">{addonName(addon())}</p>
                  <p class="text-xs text-neutral-400">
                    {t("settings.addons.manifestServed", {
                      types: addonTypes(addon()),
                    })}
                  </p>
                  <Show when={isCinemeta(addon())}>
                    <p class="text-xs text-neutral-500">
                      {t("settings.addons.cinemetaProtected")}
                    </p>
                  </Show>
                </div>
                <Focusable
                  id={`settings-addon-${addon().id}-toggle`}
                  onActivate={() => setEnabled(addon().id, !addon().enabled)}
                >
                  {({ showRing, ref, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      onClick={onClick}
                      data-testid={`settings-addon-${addon().id}-toggle`}
                      class={`rounded-md px-3 py-1 text-sm ${
                        addon().enabled
                          ? "bg-emerald-700 text-neutral-50"
                          : "bg-neutral-700 text-neutral-300"
                      } ${showRing() ? "outline outline-2 outline-sky-400" : ""}`}
                    >
                      {addon().enabled
                        ? t("settings.addons.disable")
                        : t("settings.addons.enable")}
                    </button>
                  )}
                </Focusable>
                <Focusable
                  id={`settings-addon-${addon().id}-up`}
                  onActivate={() => move(addon().id, "up")}
                >
                  {({ showRing, ref, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      onClick={onClick}
                      aria-label={t("settings.addons.moveUp")}
                      data-testid={`settings-addon-${addon().id}-up`}
                      class={`rounded-md bg-neutral-800 px-2 py-1 text-sm text-neutral-100 ${
                        showRing() ? "outline outline-2 outline-sky-400" : ""
                      }`}
                    >
                      ↑
                    </button>
                  )}
                </Focusable>
                <Focusable
                  id={`settings-addon-${addon().id}-down`}
                  onActivate={() => move(addon().id, "down")}
                >
                  {({ showRing, ref, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      onClick={onClick}
                      aria-label={t("settings.addons.moveDown")}
                      data-testid={`settings-addon-${addon().id}-down`}
                      class={`rounded-md bg-neutral-800 px-2 py-1 text-sm text-neutral-100 ${
                        showRing() ? "outline outline-2 outline-sky-400" : ""
                      }`}
                    >
                      ↓
                    </button>
                  )}
                </Focusable>
                <Show when={!isCinemeta(addon())}>
                  <Focusable
                    id={`settings-addon-${addon().id}-uninstall`}
                    onActivate={() => {
                      const a = addon();
                      props.requestConfirm(
                        t("settings.addons.uninstallConfirm", {
                          name: addonName(a),
                        }),
                        // The closure is invoked from the modal's confirm
                        // button click, not as a Solid tracked scope; the
                        // rule flags it because `uninstall` transitively
                        // reads `props`.
                        // eslint-disable-next-line solid/reactivity
                        async () => {
                          await uninstall(a);
                        },
                      );
                    }}
                  >
                    {({ showRing, ref, onClick }) => (
                      <button
                        ref={ref as (el: HTMLButtonElement) => void}
                        onClick={onClick}
                        data-testid={`settings-addon-${addon().id}-uninstall`}
                        class={`rounded-md bg-red-700 px-3 py-1 text-sm text-neutral-50 ${
                          showRing() ? "outline outline-2 outline-sky-400" : ""
                        }`}
                      >
                        {t("settings.addons.uninstall")}
                      </button>
                    )}
                  </Focusable>
                </Show>
              </li>
            )}
          </Index>
        </ul>
      </Show>

      <div class="flex flex-col gap-2">
        <label class="text-sm font-semibold text-neutral-300">
          {t("settings.addons.addByUrl")}
        </label>
        <div class="flex flex-wrap items-center gap-2">
          <Focusable id="settings-addon-add-url-input">
            {({ showRing, ref }) => (
              <input
                ref={ref as (el: HTMLInputElement) => void}
                type="url"
                value={addUrl()}
                onInput={(e) => setAddUrl(e.currentTarget.value)}
                placeholder={t("settings.addons.addByUrlPlaceholder")}
                data-testid="settings-addon-add-url-input"
                class={`flex-1 rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              />
            )}
          </Focusable>
          <Focusable id="settings-addon-add-url-submit" onActivate={addByUrl}>
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                disabled={installing()}
                data-testid="settings-addon-add-url-submit"
                class={`rounded-md bg-sky-600 px-3 py-2 text-sm font-semibold text-neutral-50 hover:bg-sky-500 disabled:opacity-50 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              >
                {installing()
                  ? t("settings.addons.installing")
                  : t("settings.addons.add")}
              </button>
            )}
          </Focusable>
        </div>
      </div>

      <Show when={props.recommended().length > 0}>
        <div class="flex flex-col gap-2">
          <h3 class="text-sm font-semibold text-neutral-300">
            {t("settings.addons.recommended")}
          </h3>
          <ul class="flex flex-col gap-2" data-testid="settings-recommended-list">
            <Index each={props.recommended()}>
              {(rec) => {
                const isInstalled = createMemo(() =>
                  props.addons().some((a) => a.manifest_url === rec().manifest_url),
                );
                return (
                  <li class="flex flex-wrap items-center gap-2 rounded-md border border-neutral-800 bg-neutral-950 p-3">
                    <div class="flex-1">
                      <p class="font-semibold text-neutral-100">{rec().name}</p>
                      <p class="text-xs text-neutral-400">{rec().description}</p>
                    </div>
                    <Show when={!isInstalled()}>
                      <Focusable
                        id={`settings-recommended-${rec().name}-install`}
                        onActivate={() => installRecommended(rec())}
                      >
                        {({ showRing, ref, onClick }) => (
                          <button
                            ref={ref as (el: HTMLButtonElement) => void}
                            onClick={onClick}
                            data-testid={`settings-recommended-${rec().name}-install`}
                            class={`rounded-md bg-emerald-700 px-3 py-1 text-sm text-neutral-50 ${
                              showRing() ? "outline outline-2 outline-sky-400" : ""
                            }`}
                          >
                            {t("settings.addons.installOne")}
                          </button>
                        )}
                      </Focusable>
                    </Show>
                  </li>
                );
              }}
            </Index>
          </ul>
        </div>
      </Show>
    </SectionShell>
  );
};

const LanguageSection: Component<SectionProps> = (props) => {
  const fallback = createMemo(() => props.view().language.metadata_fallback);

  async function persistFallback(next: string[]): Promise<void> {
    await props.persist(SETTING_KEYS.metaFallbackLangs, JSON.stringify(next));
  }

  return (
    <SectionShell id="language" titleKey="settings.sections.language">
      <FieldShell
        id="settings-section-language-primary"
        labelKey="settings.language.primary"
        hintKey="settings.language.primaryHint"
      >
        <TextField
          id="settings-section-language-primary-input"
          value={() => props.view().language.metadata_primary}
          onSave={(v) => props.persist(SETTING_KEYS.metaPrimaryLang, v)}
          placeholder="en"
        />
      </FieldShell>

      <FieldShell
        id="settings-section-language-fallback"
        labelKey="settings.language.fallback"
        hintKey="settings.language.fallbackHint"
      >
        <ul class="flex flex-col gap-2" data-testid="settings-fallback-list">
          <Index each={fallback()}>
            {(lang, idx) => (
              <li class="flex items-center gap-2">
                <Focusable id={`settings-fallback-${idx}-input`}>
                  {({ showRing, ref }) => (
                    <input
                      ref={ref as (el: HTMLInputElement) => void}
                      type="text"
                      value={lang()}
                      onChange={async (e) => {
                        const value = e.currentTarget.value.trim();
                        const next = [...fallback()];
                        if (!value) {
                          next.splice(idx, 1);
                        } else {
                          next[idx] = value;
                        }
                        await persistFallback(next);
                      }}
                      data-testid={`settings-fallback-${idx}-input`}
                      class={`w-32 rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
                        showRing() ? "outline outline-2 outline-sky-400" : ""
                      }`}
                    />
                  )}
                </Focusable>
                <Focusable
                  id={`settings-fallback-${idx}-remove`}
                  onActivate={() => {
                    const next = [...fallback()];
                    next.splice(idx, 1);
                    void persistFallback(next);
                  }}
                >
                  {({ showRing, ref, onClick }) => (
                    <button
                      ref={ref as (el: HTMLButtonElement) => void}
                      onClick={onClick}
                      data-testid={`settings-fallback-${idx}-remove`}
                      class={`rounded-md bg-red-700 px-2 py-1 text-sm text-neutral-50 ${
                        showRing() ? "outline outline-2 outline-sky-400" : ""
                      }`}
                    >
                      ×
                    </button>
                  )}
                </Focusable>
              </li>
            )}
          </Index>
        </ul>
        <Show when={fallback().length < 3}>
          <Focusable
            id="settings-fallback-add"
            onActivate={() => {
              const next = [...fallback(), "en"];
              void persistFallback(next);
            }}
          >
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="settings-fallback-add"
                class={`self-start rounded-md bg-neutral-800 px-3 py-1 text-sm text-neutral-50 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              >
                {t("settings.language.addFallback")}
              </button>
            )}
          </Focusable>
        </Show>
      </FieldShell>

      <FieldShell
        id="settings-section-language-ui"
        labelKey="settings.language.ui"
      >
        <Dropdown
          id="settings-section-language-ui-select"
          value={() => props.view().language.ui || "auto"}
          options={[
            { value: "auto", labelKey: "settings.language.uiAuto" },
            { value: "en", labelKey: "settings.language.uiEn" },
            { value: "fr", labelKey: "settings.language.uiFr" },
          ]}
          onChange={(next) => {
            // "auto" is persisted as the empty string so the same key
            // doubles as the "no explicit preference" signal.
            const persistValue = next === "auto" ? "" : next;
            void (async () => {
              await props.persist(SETTING_KEYS.uiLang, persistValue);
              if (next === "en" || next === "fr") {
                setLocale(next as SupportedLocale);
              }
            })();
          }}
        />
      </FieldShell>
    </SectionShell>
  );
};

const CacheSection: Component<
  SectionProps & {
    requestConfirm: (message: string, fn: () => Promise<void>) => void;
  }
> = (props) => {
  const [usage, { refetch: refetchUsage }] = createResource<number | null>(
    async () => {
      if (!hasTauri()) return 0;
      try {
        return await cacheUsageBytes();
      } catch {
        return null;
      }
    },
  );

  async function clearCache(): Promise<void> {
    if (hasTauri()) {
      await cacheClear();
    }
    await refetchUsage();
  }

  return (
    <SectionShell id="cache" titleKey="settings.sections.cache">
      <FieldShell
        id="settings-section-cache-path"
        labelKey="settings.cache.path"
        hintKey="settings.cache.pathHint"
      >
        <TextField
          id="settings-section-cache-path-input"
          value={() => props.view().cache.path}
          onSave={(v) => props.persist(SETTING_KEYS.cachePath, v)}
          placeholder={props.view().cache.path}
        />
      </FieldShell>
      <FieldShell
        id="settings-section-cache-size"
        labelKey="settings.cache.size"
      >
        <Slider
          id="settings-section-cache-size-input"
          value={() => props.view().cache.size_gib}
          min={() => props.view().cache.min_gib}
          max={() => props.view().cache.max_gib}
          format={(n) => t("settings.cache.sizeLabel", { n: String(n) })}
          onChange={(n) => props.persist(SETTING_KEYS.cacheSizeGib, String(n))}
        />
      </FieldShell>
      <div class="flex items-center justify-between gap-4">
        <p class="text-sm text-neutral-300">
          {t("settings.cache.usage")}:{" "}
          <span data-testid="settings-cache-usage">
            <Show when={!usage.loading} fallback={t("settings.cache.usageLoading")}>
              {usage() === null
                ? "—"
                : formatBytes(usage() as number)}
            </Show>
          </span>
        </p>
        <Focusable
          id="settings-cache-clear"
          onActivate={() =>
            props.requestConfirm(t("settings.cache.clearConfirm"), clearCache)
          }
        >
          {({ showRing, ref, onClick }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              onClick={onClick}
              data-testid="settings-cache-clear"
              class={`rounded-md bg-red-700 px-3 py-1 text-sm text-neutral-50 ${
                showRing() ? "outline outline-2 outline-sky-400" : ""
              }`}
            >
              {t("settings.cache.clear")}
            </button>
          )}
        </Focusable>
      </div>
    </SectionShell>
  );
};

const BufferSection: Component<SectionProps> = (props) => {
  const [advanced, setAdvanced] = createSignal(false);
  return (
    <SectionShell id="buffer" titleKey="settings.sections.buffer">
      <FieldShell
        id="settings-section-buffer-safety"
        labelKey="settings.buffer.safetyMargin"
      >
        <NumberField
          id="settings-section-buffer-safety-input"
          value={() => props.view().buffer.safety_margin_s}
          onSave={(v) => props.persist(SETTING_KEYS.bufferSafetyMarginS, v)}
        />
      </FieldShell>
      <FieldShell
        id="settings-section-buffer-prebuffer"
        labelKey="settings.buffer.prebufferTarget"
      >
        <NumberField
          id="settings-section-buffer-prebuffer-input"
          value={() => props.view().buffer.prebuffer_target_s}
          onSave={(v) => props.persist(SETTING_KEYS.bufferPrebufferTargetS, v)}
        />
      </FieldShell>
      <Focusable
        id="settings-buffer-advanced-toggle"
        onActivate={() => setAdvanced((c) => !c)}
      >
        {({ showRing, ref, onClick }) => (
          <button
            ref={ref as (el: HTMLButtonElement) => void}
            onClick={onClick}
            data-testid="settings-buffer-advanced-toggle"
            class={`self-start rounded-md bg-neutral-800 px-3 py-1 text-sm text-neutral-50 ${
              showRing() ? "outline outline-2 outline-sky-400" : ""
            }`}
          >
            {t("settings.buffer.advanced")} {advanced() ? "▾" : "▸"}
          </button>
        )}
      </Focusable>
      <Show when={advanced()}>
        <FieldShell
          id="settings-section-buffer-piecehigh"
          labelKey="settings.buffer.pieceHigh"
        >
          <NumberField
            id="settings-section-buffer-piecehigh-input"
            value={() => props.view().buffer.piece_high_s}
            onSave={(v) => props.persist(SETTING_KEYS.bufferPieceHighS, v)}
          />
        </FieldShell>
        <FieldShell
          id="settings-section-buffer-piecemed"
          labelKey="settings.buffer.pieceMed"
        >
          <NumberField
            id="settings-section-buffer-piecemed-input"
            value={() => props.view().buffer.piece_med_s}
            onSave={(v) => props.persist(SETTING_KEYS.bufferPieceMedS, v)}
          />
        </FieldShell>
        <FieldShell
          id="settings-section-buffer-recompute"
          labelKey="settings.buffer.recomputeInterval"
        >
          <NumberField
            id="settings-section-buffer-recompute-input"
            value={() => props.view().buffer.recompute_interval_s}
            onSave={(v) =>
              props.persist(SETTING_KEYS.bufferRecomputeIntervalS, v)
            }
          />
        </FieldShell>
      </Show>
    </SectionShell>
  );
};

const PlayerSection: Component<SectionProps> = (props) => (
  <SectionShell id="player" titleKey="settings.sections.player">
    <p class="text-xs text-neutral-400">{t("settings.player.androidOnly")}</p>
    <Toggle
      id="settings-section-player-truehd"
      labelKey="settings.player.passthroughTruehd"
      value={() => props.view().player.passthrough_truehd}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughTruehd, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-dtshd"
      labelKey="settings.player.passthroughDtshd"
      value={() => props.view().player.passthrough_dtshd}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughDtshd, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-dtsx"
      labelKey="settings.player.passthroughDtsx"
      value={() => props.view().player.passthrough_dtsx}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughDtsx, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-atmos"
      labelKey="settings.player.passthroughAtmos"
      value={() => props.view().player.passthrough_atmos}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughAtmos, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-ac3"
      labelKey="settings.player.passthroughAc3"
      value={() => props.view().player.passthrough_ac3}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughAc3, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-dts"
      labelKey="settings.player.passthroughDts"
      value={() => props.view().player.passthrough_dts}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughDts, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-eac3"
      labelKey="settings.player.passthroughEac3"
      value={() => props.view().player.passthrough_eac3}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerPassthroughEac3, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-hwdecode"
      labelKey="settings.player.forceHwDecode"
      value={() => props.view().player.force_hw_decode}
      onChange={(v) =>
        props.persist(SETTING_KEYS.playerForceHwDecode, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-player-tunneling"
      labelKey="settings.player.tunneling"
      value={() => props.view().player.tunneling}
      onChange={(v) => props.persist(SETTING_KEYS.playerTunneling, boolStr(v))}
    />
  </SectionShell>
);

const DisplaySection: Component<SectionProps> = (props) => (
  <SectionShell id="display" titleKey="settings.sections.display">
    <FieldShell
      id="settings-section-display-tile"
      labelKey="settings.display.tileSize"
    >
      <Dropdown
        id="settings-section-display-tile-select"
        value={() => props.view().display.tile_size}
        options={[
          { value: "small", labelKey: "settings.display.tileSizeSmall" },
          { value: "medium", labelKey: "settings.display.tileSizeMedium" },
          { value: "large", labelKey: "settings.display.tileSizeLarge" },
        ]}
        onChange={(v) => props.persist(SETTING_KEYS.displayTileSize, v)}
      />
    </FieldShell>
    <Toggle
      id="settings-section-display-focusanim"
      labelKey="settings.display.focusAnimation"
      value={() => props.view().display.focus_animation}
      onChange={(v) =>
        props.persist(SETTING_KEYS.displayFocusAnimation, boolStr(v))
      }
    />
    <Toggle
      id="settings-section-display-nsfw"
      labelKey="settings.display.nsfw"
      value={() => props.view().display.nsfw}
      onChange={(v) => props.persist(SETTING_KEYS.displayNsfw, boolStr(v))}
    />
    <FieldShell
      id="settings-section-display-input"
      labelKey="settings.display.inputOverride"
    >
      <Dropdown
        id="settings-section-display-input-select"
        value={() => props.view().display.input_override}
        options={[
          { value: "auto", labelKey: "settings.display.inputAuto" },
          { value: "touch", labelKey: "settings.display.inputTouch" },
          { value: "dpad", labelKey: "settings.display.inputDpad" },
          { value: "kbm", labelKey: "settings.display.inputKbm" },
        ]}
        onChange={(v) => {
          void (async () => {
            await props.persist(SETTING_KEYS.displayInputOverride, v);
            setInputOverride(v as InputProfileOverride);
          })();
        }}
      />
    </FieldShell>
    <Toggle
      id="settings-section-display-hicontrast"
      labelKey="settings.display.highContrast"
      value={() => props.view().display.high_contrast}
      onChange={(v) =>
        props.persist(SETTING_KEYS.displayHighContrast, boolStr(v))
      }
    />
  </SectionShell>
);

const AboutSection: Component<{
  appInfo: Accessor<AppInfo>;
  announce: AnnounceFn;
}> = (props) => {
  const [dest, setDest] = createSignal("");

  async function doExport(): Promise<void> {
    const path = dest().trim();
    if (!path) return;
    try {
      const bytes = hasTauri() ? await exportLogs(path) : 0;
      props.announce({
        kind: "ok",
        message: t("settings.about.exportLogsSaved", {
          n: String(bytes),
          path,
        }),
      });
    } catch (err) {
      const message = String((err as { message?: string }).message ?? err);
      props.announce({
        kind: "error",
        message: t("settings.about.exportLogsFailed", { reason: message }),
      });
    }
  }

  return (
    <SectionShell id="about" titleKey="settings.sections.about">
      <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt class="text-neutral-400">{t("settings.about.version")}</dt>
        <dd
          class="font-mono text-neutral-100"
          data-testid="settings-about-version"
        >
          {props.appInfo().version}
        </dd>
        <dt class="text-neutral-400">{t("settings.about.commit")}</dt>
        <dd
          class="font-mono text-neutral-100"
          data-testid="settings-about-commit"
        >
          {props.appInfo().commit}
        </dd>
        <dt class="text-neutral-400">{t("settings.about.platform")}</dt>
        <dd class="font-mono text-neutral-100">{props.appInfo().platform}</dd>
        <dt class="text-neutral-400">{t("settings.about.license")}</dt>
        <dd class="font-mono text-neutral-100">{props.appInfo().license}</dd>
      </dl>
      <Show when={props.appInfo().repository}>
        <Focusable id="settings-about-repo">
          {({ showRing, ref }) => (
            <a
              ref={ref as (el: HTMLAnchorElement) => void}
              href={props.appInfo().repository}
              target="_blank"
              rel="noreferrer"
              data-testid="settings-about-repo"
              class={`self-start text-sm text-sky-300 underline ${
                showRing() ? "outline outline-2 outline-sky-400" : ""
              }`}
            >
              {t("settings.about.openRepo")} ↗
            </a>
          )}
        </Focusable>
      </Show>
      <div class="flex flex-col gap-2">
        <label class="text-sm font-semibold text-neutral-200">
          {t("settings.about.exportLogs")}
        </label>
        <p class="text-xs text-neutral-400">
          {t("settings.about.exportLogsHint")}
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <Focusable id="settings-about-export-input">
            {({ showRing, ref }) => (
              <input
                ref={ref as (el: HTMLInputElement) => void}
                type="text"
                value={dest()}
                onInput={(e) => setDest(e.currentTarget.value)}
                placeholder={t("settings.about.exportLogsPathPlaceholder")}
                data-testid="settings-about-export-input"
                class={`flex-1 rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              />
            )}
          </Focusable>
          <Focusable id="settings-about-export-submit" onActivate={doExport}>
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="settings-about-export-submit"
                class={`rounded-md bg-sky-600 px-3 py-2 text-sm font-semibold text-neutral-50 hover:bg-sky-500 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              >
                {t("settings.about.exportLogs")}
              </button>
            )}
          </Focusable>
        </div>
      </div>
    </SectionShell>
  );
};

// ---- shared form-control primitives -------------------------------------

const FieldShell: Component<{
  id: string;
  labelKey: string;
  hintKey?: string;
  children: JSX.Element;
}> = (props) => (
  <div class="flex flex-col gap-1" data-testid={props.id}>
    <label class="text-sm font-semibold text-neutral-200">
      {tDyn(props.labelKey)}
    </label>
    <Show when={props.hintKey}>
      <p class="text-xs text-neutral-400">{tDyn(props.hintKey!)}</p>
    </Show>
    {props.children}
  </div>
);

const TextField: Component<{
  id: string;
  value: Accessor<string>;
  onSave: (next: string) => Promise<unknown>;
  placeholder?: string;
}> = (props) => {
  const [draft, setDraft] = createSignal(untrack(() => props.value()));
  createEffect(on(() => props.value(), (next) => setDraft(next)));
  return (
    <Focusable id={props.id}>
      {({ showRing, ref }) => (
        <input
          ref={ref as (el: HTMLInputElement) => void}
          type="text"
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onChange={(e) => {
            void props.onSave(e.currentTarget.value);
          }}
          placeholder={props.placeholder}
          data-testid={props.id}
          class={`rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
            showRing() ? "outline outline-2 outline-sky-400" : ""
          }`}
        />
      )}
    </Focusable>
  );
};

const NumberField: Component<{
  id: string;
  value: Accessor<number>;
  onSave: (next: string) => Promise<unknown>;
}> = (props) => {
  const [draft, setDraft] = createSignal(String(untrack(() => props.value())));
  createEffect(on(() => props.value(), (next) => setDraft(String(next))));
  return (
    <Focusable id={props.id}>
      {({ showRing, ref }) => (
        <input
          ref={ref as (el: HTMLInputElement) => void}
          type="number"
          value={draft()}
          onInput={(e) => setDraft(e.currentTarget.value)}
          onChange={(e) => {
            void props.onSave(e.currentTarget.value);
          }}
          data-testid={props.id}
          class={`w-32 rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
            showRing() ? "outline outline-2 outline-sky-400" : ""
          }`}
        />
      )}
    </Focusable>
  );
};

const Slider: Component<{
  id: string;
  value: Accessor<number>;
  min: Accessor<number>;
  max: Accessor<number>;
  format: (n: number) => string;
  onChange: (n: number) => void | Promise<unknown>;
}> = (props) => {
  const [draft, setDraft] = createSignal(untrack(() => props.value()));
  createEffect(on(() => props.value(), (next) => setDraft(next)));
  return (
    <div class="flex items-center gap-3">
      <Focusable id={props.id}>
        {({ showRing, ref }) => (
          <input
            ref={ref as (el: HTMLInputElement) => void}
            type="range"
            min={props.min()}
            max={props.max()}
            value={draft()}
            onInput={(e) => setDraft(Number(e.currentTarget.value))}
            onChange={async (e) => {
              const n = Number(e.currentTarget.value);
              setDraft(n);
              await props.onChange(n);
            }}
            data-testid={props.id}
            class={`flex-1 ${
              showRing() ? "outline outline-2 outline-sky-400" : ""
            }`}
          />
        )}
      </Focusable>
      <span class="w-24 text-right font-mono text-sm text-neutral-200">
        {props.format(draft())}
      </span>
    </div>
  );
};

const Toggle: Component<{
  id: string;
  labelKey: string;
  value: Accessor<boolean>;
  onChange: (next: boolean) => void | Promise<unknown>;
}> = (props) => (
  <Focusable id={props.id} onActivate={() => props.onChange(!props.value())}>
    {({ showRing, ref, onClick }) => (
      <button
        ref={ref as (el: HTMLButtonElement) => void}
        onClick={onClick}
        role="switch"
        aria-checked={props.value() ? "true" : "false"}
        data-testid={props.id}
        data-state={props.value() ? "on" : "off"}
        class={`flex items-center justify-between gap-3 rounded-md border border-neutral-800 bg-neutral-950 px-3 py-2 text-left text-neutral-100 ${
          showRing() ? "outline outline-2 outline-sky-400" : ""
        }`}
      >
        <span>{tDyn(props.labelKey)}</span>
        <span
          class={`inline-flex h-5 w-10 items-center rounded-full px-1 transition-colors ${
            props.value() ? "bg-emerald-600" : "bg-neutral-700"
          }`}
        >
          <span
            class={`block h-3 w-3 rounded-full bg-white transition-transform ${
              props.value() ? "translate-x-5" : "translate-x-0"
            }`}
          />
        </span>
      </button>
    )}
  </Focusable>
);

type DropdownOption = { value: string; labelKey: string };

const Dropdown: Component<{
  id: string;
  value: Accessor<string>;
  options: readonly DropdownOption[];
  onChange: (next: string) => void | Promise<unknown>;
}> = (props) => (
  <Focusable id={props.id}>
    {({ showRing, ref }) => (
      <select
        ref={ref as (el: HTMLSelectElement) => void}
        value={props.value()}
        onChange={async (e) => {
          await props.onChange(e.currentTarget.value);
        }}
        data-testid={props.id}
        class={`w-fit rounded-md border border-neutral-700 bg-neutral-950 px-3 py-2 text-neutral-50 ${
          showRing() ? "outline outline-2 outline-sky-400" : ""
        }`}
      >
        <For each={props.options}>
          {(opt) => <option value={opt.value}>{tDyn(opt.labelKey)}</option>}
        </For>
      </select>
    )}
  </Focusable>
);

const ConfirmModal: Component<{
  message: string;
  onConfirm: () => void | Promise<void>;
  onCancel: () => void;
}> = (props) => {
  onMount(() => {
    setInitialFocus("settings-confirm-confirm");
  });
  return (
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-8"
      data-testid="settings-confirm-modal"
      role="dialog"
      aria-modal="true"
    >
      <div class="flex flex-col gap-4 rounded-lg border border-neutral-700 bg-neutral-900 p-6 shadow-2xl">
        <p class="text-neutral-100">{props.message}</p>
        <div class="flex justify-end gap-3">
          <Focusable id="settings-confirm-cancel" onActivate={props.onCancel}>
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="settings-confirm-cancel"
                class={`rounded-md bg-neutral-700 px-3 py-2 text-sm text-neutral-100 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              >
                {t("settings.cancel")}
              </button>
            )}
          </Focusable>
          <Focusable
            id="settings-confirm-confirm"
            onActivate={() => {
              void props.onConfirm();
            }}
          >
            {({ showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="settings-confirm-confirm"
                class={`rounded-md bg-red-600 px-3 py-2 text-sm font-semibold text-neutral-50 ${
                  showRing() ? "outline outline-2 outline-sky-400" : ""
                }`}
              >
                {t("settings.confirm")}
              </button>
            )}
          </Focusable>
        </div>
      </div>
    </div>
  );
};

// ---- pure helpers --------------------------------------------------------

function boolStr(b: boolean): string {
  return b ? "true" : "false";
}

/**
 * Format a byte count in the largest binary unit ≤ value, with one
 * fractional digit when the integer form is < 100.
 */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = n / 1024;
  let unit = units[0];
  for (let i = 1; i < units.length && value >= 1024; i++) {
    value /= 1024;
    unit = units[i];
  }
  return value < 100 ? `${value.toFixed(1)} ${unit}` : `${Math.round(value)} ${unit}`;
}

// Re-export the current platform helper for tests that want to assert the
// Player section gates on the host platform.
export { detectPlatform };

// Re-export so App.tsx can read the persisted UI language on boot.
export { locale, inputOverride };
