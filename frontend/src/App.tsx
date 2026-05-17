import {
  createSignal,
  onCleanup,
  onMount,
  type Component,
} from "solid-js";

import { Focusable } from "./components/Focusable";
import {
  installInputSubsystem,
  onAction,
  profile,
  type Action,
  type InputSource,
} from "./input";
import { t } from "./i18n";

/**
 * Placeholder shell. PRD §F-001 acceptance is still "App launches and
 * shows a placeholder home screen with the text 'kino'"; that
 * promise is preserved by rendering the kino title at the top. The
 * lower half of the page is the F-017 input demonstrator the
 * acceptance test "UI responds correctly to mocked input events"
 * binds against. The real 10-foot UI (F-008) replaces this view
 * end-to-end.
 */
const App: Component = () => {
  const [lastAction, setLastAction] = createSignal<
    { action: Action; source: InputSource } | null
  >(null);

  onMount(() => {
    const uninstallInput = installInputSubsystem();
    const unsubscribe = onAction((action, source) => {
      setLastAction({ action, source });
    });
    onCleanup(() => {
      unsubscribe();
      uninstallInput();
    });
  });

  const profileLabel = () => {
    switch (profile()) {
      case "touch":
        return t("input.profileTouch");
      case "dpad":
        return t("input.profileDpad");
      case "kbm":
        return t("input.profileKbm");
      case "gamepad":
        return t("input.profileGamepad");
    }
  };

  return (
    <main
      class="flex h-full w-full flex-col items-center justify-center gap-6 bg-neutral-950 p-8 text-neutral-50"
      role="main"
    >
      <h1 class="text-7xl font-bold tracking-tight" data-testid="home-title">
        {t("app.placeholderHome")}
      </h1>
      <p class="text-lg text-neutral-400" data-testid="home-tagline">
        {t("app.placeholderTagline")}
      </p>
      <section
        class="mt-6 flex flex-col items-center gap-3 rounded-md border border-neutral-800 p-4 text-sm"
        data-testid="input-demo"
      >
        <div data-testid="input-profile">
          <span class="text-neutral-400">{t("input.profileLabel")}: </span>
          <span class="font-medium">{profileLabel()}</span>
        </div>
        <div data-testid="input-last-action">
          <span class="text-neutral-400">{t("input.lastActionLabel")}: </span>
          <span class="font-mono">
            {lastAction()
              ? `${lastAction()!.action} (${lastAction()!.source})`
              : t("input.lastActionNone")}
          </span>
        </div>
        <p class="max-w-md text-center text-neutral-500">
          {t("input.demoHint")}
        </p>
        <div class="flex gap-2">
          <Focusable id="demo-tile-1">
            {({ ref, showRing, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="demo-tile-1"
                class={`rounded-md px-4 py-2 transition-transform duration-150 ease-out ${
                  showRing()
                    ? "scale-105 bg-neutral-800 ring-2 ring-sky-400"
                    : "bg-neutral-800"
                }`}
              >
                tile 1
              </button>
            )}
          </Focusable>
          <Focusable id="demo-tile-2">
            {({ ref, showRing, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="demo-tile-2"
                class={`rounded-md px-4 py-2 transition-transform duration-150 ease-out ${
                  showRing()
                    ? "scale-105 bg-neutral-800 ring-2 ring-sky-400"
                    : "bg-neutral-800"
                }`}
              >
                tile 2
              </button>
            )}
          </Focusable>
          <Focusable id="demo-tile-3">
            {({ ref, showRing, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid="demo-tile-3"
                class={`rounded-md px-4 py-2 transition-transform duration-150 ease-out ${
                  showRing()
                    ? "scale-105 bg-neutral-800 ring-2 ring-sky-400"
                    : "bg-neutral-800"
                }`}
              >
                tile 3
              </button>
            )}
          </Focusable>
        </div>
      </section>
    </main>
  );
};

export default App;
