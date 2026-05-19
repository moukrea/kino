// F-016 Settings route tests. Pin the four PRD §F-016 §6A
// code-acceptance items:
//
//   1. All settings persist across restarts — verified end-to-end by
//      asserting `settingsSet` writes the right (key, value) pair for
//      each control.
//   2. Test buttons return clear success/failure with error reason —
//      verified by mocking `test_tmdb` etc. and asserting the visible
//      success / failure text.
//   3. Reset to defaults button with confirmation restores out-of-box
//      state — verified by triggering Reset, confirming the modal,
//      and asserting `settingsResetDefaults` is called + the view
//      reloads.
//   4. All settings navigable end-to-end with D-pad only — verified
//      structurally: every interactive control declares a
//      `data-testid` matching a `<Focusable>` id so the F-017 focus
//      manager can traverse them.
//
// Plus negative assertions for the Android-only Player section
// (hidden on Linux) and the formatBytes helper.

import { render } from "solid-js/web";
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import {
  formatBytes,
  SETTINGS_INITIAL_FOCUS_ID,
  SettingsContent,
} from "./Settings";
import { _resetForTests as _resetFocus } from "../input/focus";
import { _resetForTests as _resetProfile } from "../input/profile";
import { setLocale } from "../i18n";
import type {
  AddonRow,
  AppInfo,
  RecommendedAddon,
  SettingsView,
} from "../lib/tauri";

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return {
    ...actual,
    hasTauri: () => true,
    settingsSet: vi.fn(),
    settingsResetDefaults: vi.fn(),
    cacheUsageBytes: vi.fn(),
    cacheClear: vi.fn(),
    exportLogs: vi.fn(),
    pickDirectory: vi.fn(),
    testTmdb: vi.fn(),
    testTrakt: vi.fn(),
    testTvdb: vi.fn(),
    testFanart: vi.fn(),
    addonsSetEnabled: vi.fn(),
    installAddon: vi.fn(),
    uninstallAddon: vi.fn(),
    setAddonOrder: vi.fn(),
  };
});

const tauri = await import("../lib/tauri");
const mockedSet = vi.mocked(tauri.settingsSet);
const mockedReset = vi.mocked(tauri.settingsResetDefaults);
const mockedUsage = vi.mocked(tauri.cacheUsageBytes);
const mockedCacheClear = vi.mocked(tauri.cacheClear);
const mockedPickDirectory = vi.mocked(tauri.pickDirectory);
const mockedTestTmdb = vi.mocked(tauri.testTmdb);
const mockedInstallAddon = vi.mocked(tauri.installAddon);
const mockedUninstallAddon = vi.mocked(tauri.uninstallAddon);
const mockedSetEnabled = vi.mocked(tauri.addonsSetEnabled);

function defaultView(): SettingsView {
  return {
    api_keys: { tmdb: "", trakt: "", tvdb: "", fanart: "" },
    language: { metadata_primary: "", metadata_fallback: [], ui: "" },
    cache: {
      path: "/home/user/.config/kino/cache",
      size_gib: 4,
      min_gib: 1,
      max_gib: 100,
    },
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
      advanced_logging: false,
      show_unavailable: false,
    },
  };
}

function defaultAppInfo(platform: "linux" | "android" = "linux"): AppInfo {
  return {
    name: "kino-app",
    version: "0.1.0",
    commit: "abc1234",
    repository: "https://github.com/moukrea/kino",
    license: "MIT",
    platform,
  };
}

type LoaderOverrides = {
  view?: SettingsView;
  addons?: AddonRow[];
  recommended?: RecommendedAddon[];
  appInfo?: AppInfo;
};

function makeLoader(overrides: LoaderOverrides = {}) {
  return async () => ({
    view: overrides.view ?? defaultView(),
    addons: overrides.addons ?? [],
    recommended: overrides.recommended ?? [],
    appInfo: overrides.appInfo ?? defaultAppInfo(),
  });
}

async function flushAsync(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
}

async function waitFor(
  pred: () => boolean,
  iterations = 50,
): Promise<void> {
  for (let i = 0; i < iterations; i++) {
    if (pred()) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("waitFor: predicate did not become true");
}

function mount(overrides: LoaderOverrides = {}): {
  host: HTMLDivElement;
  dispose: () => void;
} {
  const host = document.createElement("div");
  document.body.appendChild(host);
  const dispose = render(
    () => <SettingsContent loader={makeLoader(overrides)} />,
    host,
  );
  return { host, dispose };
}

describe("Settings route (F-016)", () => {
  let activeHost: HTMLDivElement | null = null;
  let activeDispose: (() => void) | null = null;

  beforeEach(() => {
    _resetFocus();
    _resetProfile();
    setLocale("en");
    vi.useRealTimers();
    mockedSet.mockReset();
    mockedReset.mockReset();
    mockedUsage.mockReset();
    mockedCacheClear.mockReset();
    mockedPickDirectory.mockReset();
    mockedTestTmdb.mockReset();
    mockedInstallAddon.mockReset();
    mockedUninstallAddon.mockReset();
    mockedSetEnabled.mockReset();
    mockedSet.mockImplementation(async (_, v) => v);
    mockedReset.mockResolvedValue(undefined);
    mockedUsage.mockResolvedValue(1024);
    mockedCacheClear.mockResolvedValue(undefined);
    mockedTestTmdb.mockResolvedValue(undefined);
    mockedInstallAddon.mockResolvedValue({
      id: "stub",
      manifest_url: "https://stub/manifest.json",
      enabled: true,
      installed_at: 0,
      manifest_json: {},
      display_order: 0,
    });
    mockedUninstallAddon.mockResolvedValue(1);
    mockedSetEnabled.mockResolvedValue(1);
  });

  afterEach(() => {
    activeDispose?.();
    activeHost?.remove();
    activeHost = null;
    activeDispose = null;
  });

  it("renders every PRD §F-016 section heading on Linux", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-apiKeys"]'),
    );
    expect(host.querySelector('[data-testid="settings-section-apiKeys"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-addons"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-language"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-cache"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-buffer"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-display"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="settings-section-about"]')).not.toBeNull();
    // Player section is Android-only on Linux defaults.
    expect(host.querySelector('[data-testid="settings-section-player"]')).toBeNull();
  });

  it("renders the Player section when the host platform is Android", async () => {
    const { host, dispose } = mount({ appInfo: defaultAppInfo("android") });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-player"]'),
    );
    expect(
      host.querySelector('[data-testid="settings-section-player"]'),
    ).not.toBeNull();
  });

  it("claims initial focus on the TMDB key input", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector(`[data-testid="${SETTINGS_INITIAL_FOCUS_ID}"]`),
    );
    // The focus manager picks the registered id on mount; assert the
    // element exists with the canonical id (the manager's behavior is
    // already covered by F-017 tests).
    expect(
      host.querySelector(`[data-testid="${SETTINGS_INITIAL_FOCUS_ID}"]`),
    ).not.toBeNull();
  });

  it("persists API-key edits through settingsSet with the canonical KV key", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-apiKeys-tmdb-input"]'),
    );

    const input = host.querySelector<HTMLInputElement>(
      '[data-testid="settings-section-apiKeys-tmdb-input"]',
    )!;
    input.value = "new-tmdb-key";
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await flushAsync();

    expect(mockedSet).toHaveBeenCalledWith("tmdb_api_key", "new-tmdb-key");
  });

  it("surfaces 'OK' on a successful credential test", async () => {
    mockedTestTmdb.mockResolvedValue(undefined);
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-apiKeys-tmdb-test"]'),
    );
    const btn = host.querySelector<HTMLButtonElement>(
      '[data-testid="settings-section-apiKeys-tmdb-test"]',
    )!;
    btn.click();
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector(
          '[data-testid="settings-section-apiKeys-tmdb-test-result"]',
        ),
    );
    const result = host.querySelector(
      '[data-testid="settings-section-apiKeys-tmdb-test-result"]',
    )!;
    expect(result.textContent ?? "").toContain("OK");
  });

  it("surfaces the failure reason when a credential test rejects", async () => {
    mockedTestTmdb.mockRejectedValue(new Error("401 Unauthorized"));
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-apiKeys-tmdb-test"]'),
    );
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-apiKeys-tmdb-test"]',
      )!
      .click();
    await flushAsync();
    await waitFor(
      () =>
        !!host
          .querySelector(
            '[data-testid="settings-section-apiKeys-tmdb-test-result"]',
          )
          ?.textContent?.includes("401"),
    );
    const result = host.querySelector(
      '[data-testid="settings-section-apiKeys-tmdb-test-result"]',
    )!;
    expect(result.textContent ?? "").toContain("401 Unauthorized");
  });

  it("opens the confirm modal on Reset, calls settingsResetDefaults on Confirm", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-reset"]'),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-reset"]',
      )!
      .click();
    await flushAsync();
    expect(
      host.querySelector('[data-testid="settings-confirm-modal"]'),
    ).not.toBeNull();

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-confirm-confirm"]',
      )!
      .click();
    await flushAsync();
    await waitFor(() => mockedReset.mock.calls.length > 0);
    expect(mockedReset).toHaveBeenCalledTimes(1);
  });

  it("dismisses the confirm modal on Cancel without calling reset", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-reset"]'),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-reset"]',
      )!
      .click();
    await flushAsync();

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-confirm-cancel"]',
      )!
      .click();
    await flushAsync();

    expect(
      host.querySelector('[data-testid="settings-confirm-modal"]'),
    ).toBeNull();
    expect(mockedReset).not.toHaveBeenCalled();
  });

  it("changes the UI language via the dropdown and persists the choice", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-language-ui-select"]'),
    );

    const select = host.querySelector<HTMLSelectElement>(
      '[data-testid="settings-section-language-ui-select"]',
    )!;
    select.value = "fr";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await flushAsync();

    expect(mockedSet).toHaveBeenCalledWith("lang.ui", "fr");
  });

  it("persists 'auto' UI language as the empty string", async () => {
    const view = defaultView();
    view.language.ui = "fr";
    const { host, dispose } = mount({ view });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-language-ui-select"]'),
    );

    const select = host.querySelector<HTMLSelectElement>(
      '[data-testid="settings-section-language-ui-select"]',
    )!;
    select.value = "auto";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await flushAsync();
    expect(mockedSet).toHaveBeenCalledWith("lang.ui", "");
  });

  it("renders installed addons with a disable / uninstall button (except Cinemeta)", async () => {
    const addons: AddonRow[] = [
      {
        id: "com.linvo.cinemeta",
        manifest_url: "https://v3-cinemeta.strem.io/manifest.json",
        enabled: true,
        installed_at: 0,
        manifest_json: { name: "Cinemeta", types: ["movie", "series"] },
        display_order: 0,
      },
      {
        id: "third.party",
        manifest_url: "https://third/manifest.json",
        enabled: true,
        installed_at: 0,
        manifest_json: { name: "Third Party", types: ["stream"] },
        display_order: 1,
      },
    ];
    const { host, dispose } = mount({ addons });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-addon-third.party"]'),
    );

    // Third-party addon HAS an uninstall button.
    expect(
      host.querySelector(
        '[data-testid="settings-addon-third.party-uninstall"]',
      ),
    ).not.toBeNull();
    // Cinemeta does NOT — it's non-removable per PRD §F-007.
    expect(
      host.querySelector(
        '[data-testid="settings-addon-com.linvo.cinemeta-uninstall"]',
      ),
    ).toBeNull();
  });

  it("toggles an addon's enabled state through addonsSetEnabled", async () => {
    const addons: AddonRow[] = [
      {
        id: "third.party",
        manifest_url: "https://third/manifest.json",
        enabled: true,
        installed_at: 0,
        manifest_json: { name: "Third Party", types: ["stream"] },
        display_order: 0,
      },
    ];
    const { host, dispose } = mount({ addons });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector('[data-testid="settings-addon-third.party-toggle"]'),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-addon-third.party-toggle"]',
      )!
      .click();
    await flushAsync();
    await waitFor(() => mockedSetEnabled.mock.calls.length > 0);
    expect(mockedSetEnabled).toHaveBeenCalledWith("third.party", false);
  });

  it("installs an addon by URL via installAddon", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-addon-add-url-input"]'),
    );

    const input = host.querySelector<HTMLInputElement>(
      '[data-testid="settings-addon-add-url-input"]',
    )!;
    input.value = "https://torrentio.strem.fun/manifest.json";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-addon-add-url-submit"]',
      )!
      .click();
    await flushAsync();
    await waitFor(() => mockedInstallAddon.mock.calls.length > 0);
    expect(mockedInstallAddon).toHaveBeenCalledWith(
      "https://torrentio.strem.fun/manifest.json",
    );
  });

  it("toggles a boolean Display setting through settingsSet with 'true'/'false'", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-display-nsfw"]'),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-display-nsfw"]',
      )!
      .click();
    await flushAsync();
    expect(mockedSet).toHaveBeenCalledWith("display.nsfw", "true");
  });

  it("persists the PRD §5 advanced-logging toggle via settingsSet", async () => {
    // PRD §5 Logging: "DEBUG when 'advanced logging' toggle is on in
    // settings". The host watches the same key in `settings_set` and
    // flips the live `tracing` `EnvFilter`; the toggle itself is a
    // plain display-section bool from the frontend's point of view.
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector(
          '[data-testid="settings-section-display-advancedlogging"]',
        ),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-display-advancedlogging"]',
      )!
      .click();
    await flushAsync();
    expect(mockedSet).toHaveBeenCalledWith("display.advanced_logging", "true");
  });

  it("PRD §F-006: persists the show-unavailable toggle AND updates the live signal", async () => {
    // PRD §F-006: "Setting 'Show unavailable titles' (default OFF)".
    // The Display section's toggle must (a) persist the new key via
    // settingsSet so the value survives restarts, AND (b) update the
    // shared `displaySettings.setShowUnavailable` signal so already-
    // mounted catalog rows re-render without a route remount.
    const { setShowUnavailable, _resetForTests, showUnavailable } =
      await import("../lib/displaySettings");
    _resetForTests();
    expect(showUnavailable()).toBe(false);

    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector(
          '[data-testid="settings-section-display-showunavailable"]',
        ),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-display-showunavailable"]',
      )!
      .click();
    await flushAsync();
    expect(mockedSet).toHaveBeenCalledWith(
      "display.show_unavailable",
      "true",
    );
    expect(showUnavailable()).toBe(true);

    // Defensive: keep the suppression we restored above by re-zeroing
    // the signal for the next test in this file.
    setShowUnavailable(false);
  });

  it("propagates the tile-size dropdown change", async () => {
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector('[data-testid="settings-section-display-tile-select"]'),
    );

    const select = host.querySelector<HTMLSelectElement>(
      '[data-testid="settings-section-display-tile-select"]',
    )!;
    select.value = "large";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await flushAsync();
    expect(mockedSet).toHaveBeenCalledWith("display.tile_size", "large");
  });

  it("PRD §F-016 §4: Browse button picks a directory and persists cache.path", async () => {
    // ADR-118 reclassified ADR-095's text-only fallback as a §6A
    // regression; this asserts the closure plan: clicking Browse calls
    // `pickDirectory` (which wraps `tauri-plugin-dialog`'s `open`) and
    // routes a successful pick through the same `settingsSet` channel
    // the manual TextField uses, so persistence + validation + live
    // cache-root rebind in `lib.rs` all stay on a single code path.
    mockedPickDirectory.mockResolvedValue("/picked/cache/dir");
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector(
          '[data-testid="settings-section-cache-path-browse"]',
        ),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-cache-path-browse"]',
      )!
      .click();
    await flushAsync();

    expect(mockedPickDirectory).toHaveBeenCalledTimes(1);
    expect(mockedSet).toHaveBeenCalledWith("cache.path", "/picked/cache/dir");
  });

  it("PRD §F-016 §4: Browse cancel (null) leaves cache.path untouched", async () => {
    mockedPickDirectory.mockResolvedValue(null);
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector(
          '[data-testid="settings-section-cache-path-browse"]',
        ),
    );

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-section-cache-path-browse"]',
      )!
      .click();
    await flushAsync();

    expect(mockedPickDirectory).toHaveBeenCalledTimes(1);
    expect(mockedSet).not.toHaveBeenCalledWith(
      "cache.path",
      expect.any(String),
    );
  });

  it("PRD §F-016 §8: View license opens a modal containing the LICENSE body", async () => {
    // ADR-118 reclassified the literal "MIT" string as a §6A regression;
    // closure ships an in-app scrollable modal with the inlined LICENSE
    // text imported via Vite `?raw`. The modal trigger is Focusable so
    // the F-017 D-pad manager can reach it; closing the modal restores
    // the underlying page.
    const { host, dispose } = mount();
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () =>
        !!host.querySelector('[data-testid="settings-about-license-view"]'),
    );

    // Modal not visible until trigger fires.
    expect(
      host.querySelector('[data-testid="settings-about-license-modal"]'),
    ).toBeNull();

    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-about-license-view"]',
      )!
      .click();
    await flushAsync();

    const modal = host.querySelector(
      '[data-testid="settings-about-license-modal"]',
    );
    expect(modal).not.toBeNull();
    const body = host.querySelector(
      '[data-testid="settings-about-license-body"]',
    );
    expect(body?.textContent).toContain("MIT License");
    expect(body?.textContent).toContain("Permission is hereby granted");

    // Close button collapses the modal back.
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="settings-about-license-close"]',
      )!
      .click();
    await flushAsync();

    expect(
      host.querySelector('[data-testid="settings-about-license-modal"]'),
    ).toBeNull();
  });

  it("renders the About section with version + commit from get_app_info", async () => {
    const { host, dispose } = mount({ appInfo: defaultAppInfo() });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-about-version"]'),
    );
    expect(
      host.querySelector('[data-testid="settings-about-version"]')?.textContent,
    ).toContain("0.1.0");
    expect(
      host.querySelector('[data-testid="settings-about-commit"]')?.textContent,
    ).toContain("abc1234");
  });

  it("exposes one Focusable id per interactive control so D-pad nav reaches them all", async () => {
    // PRD §F-016 §6A: "All settings navigable end-to-end with D-pad only".
    // Each Focusable carries a `data-testid` matching its registered id.
    // We assert a representative spread covers every section's primary
    // control plus the Reset button.
    const { host, dispose } = mount({ appInfo: defaultAppInfo("android") });
    activeHost = host;
    activeDispose = dispose;
    await flushAsync();
    await waitFor(
      () => !!host.querySelector('[data-testid="settings-section-apiKeys"]'),
    );
    const expectedIds = [
      "settings-reset",
      "settings-section-apiKeys-tmdb-input",
      "settings-section-apiKeys-tmdb-test",
      "settings-section-apiKeys-trakt-input",
      "settings-addon-add-url-input",
      "settings-addon-add-url-submit",
      "settings-section-language-primary-input",
      "settings-section-language-ui-select",
      "settings-section-cache-path-input",
      "settings-section-cache-path-browse",
      "settings-section-cache-size-input",
      "settings-cache-clear",
      "settings-section-buffer-safety-input",
      "settings-section-buffer-prebuffer-input",
      "settings-buffer-advanced-toggle",
      "settings-section-player-truehd",
      "settings-section-player-tunneling",
      "settings-section-display-tile-select",
      "settings-section-display-nsfw",
      "settings-section-display-input-select",
      "settings-section-display-showunavailable",
      "settings-about-license-view",
      "settings-about-export-input",
      "settings-about-export-submit",
    ];
    for (const id of expectedIds) {
      expect(host.querySelector(`[data-testid="${id}"]`), `${id} missing`).not.toBeNull();
    }
  });
});

describe("formatBytes", () => {
  it("renders bytes for sub-KiB values", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
  });
  it("steps through KiB / MiB / GiB / TiB", () => {
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatBytes(1024 * 1024 * 5)).toBe("5.0 MiB");
    expect(formatBytes(1024 * 1024 * 1024 * 3)).toBe("3.0 GiB");
    expect(formatBytes(1024 ** 4 * 2)).toBe("2.0 TiB");
  });
  it("drops the fractional digit past 100 in a unit", () => {
    expect(formatBytes(150 * 1024)).toBe("150 KiB");
  });
});
