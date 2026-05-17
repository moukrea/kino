// Left-hand nav rail (PRD §F-008):
//
//   - Items: Home, Movies, Series, Search, Settings
//   - Default collapsed (icons only); expands on focus or hover
//   - Each item is a Focusable so D-pad / arrow / gamepad traversal
//     reaches it
//
// The expanded vs collapsed visual is driven by two signals OR'd
// together: a `hovered` signal flipped by `onMouseEnter` /
// `onMouseLeave`, and a `focusInside` signal flipped when any nav-rail
// item gains focus. Either signal keeps the rail open; both off
// returns it to the collapsed state.

import {
  createMemo,
  createSignal,
  For,
  type Component,
} from "solid-js";
import { useNavigate, useLocation } from "@solidjs/router";

import { Focusable } from "./Focusable";
import { t } from "../i18n";

type NavItem = {
  id: string;
  path: string;
  labelKey: "nav.home" | "nav.movies" | "nav.series" | "nav.search" | "nav.settings";
  /** Single-character glyph rendered in the collapsed state. */
  icon: string;
};

const NAV_ITEMS: readonly NavItem[] = [
  { id: "home", path: "/", labelKey: "nav.home", icon: "⌂" },
  { id: "movies", path: "/movies", labelKey: "nav.movies", icon: "▶" },
  { id: "series", path: "/series", labelKey: "nav.series", icon: "▦" },
  { id: "search", path: "/search", labelKey: "nav.search", icon: "⌕" },
  { id: "settings", path: "/settings", labelKey: "nav.settings", icon: "⚙" },
] as const;

export const NavRail: Component = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const [hovered, setHovered] = createSignal(false);
  const [focusedItem, setFocusedItem] = createSignal<string | null>(null);

  const expanded = createMemo(() => hovered() || focusedItem() !== null);

  const isActive = (path: string) =>
    path === "/"
      ? location.pathname === "/"
      : location.pathname === path || location.pathname.startsWith(`${path}/`);

  return (
    <nav
      class={`flex h-full flex-shrink-0 flex-col gap-2 border-r border-neutral-800 bg-neutral-950/95 py-6 transition-[width] duration-150 ease-out ${
        expanded() ? "w-56" : "w-16"
      }`}
      data-testid="nav-rail"
      data-expanded={expanded() ? "true" : "false"}
      aria-label={t("nav.label")}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <For each={NAV_ITEMS}>
        {(item) => (
          <Focusable
            id={`nav-${item.id}`}
            onActivate={() => navigate(item.path)}
            onFocus={() => setFocusedItem(item.id)}
            onBlur={() =>
              setFocusedItem((current) => (current === item.id ? null : current))
            }
          >
            {({ focused, showRing, ref, onClick }) => (
              <button
                ref={ref as (el: HTMLButtonElement) => void}
                onClick={onClick}
                data-testid={`nav-item-${item.id}`}
                data-active={isActive(item.path) ? "true" : "false"}
                data-focused={focused() ? "true" : "false"}
                class={`mx-2 flex items-center gap-3 rounded-md px-3 py-2 text-left transition-colors duration-150 ease-out ${
                  isActive(item.path)
                    ? "bg-neutral-800 text-neutral-50"
                    : "text-neutral-300 hover:bg-neutral-900"
                } ${showRing() ? "outline outline-2 outline-sky-400" : ""}`}
              >
                <span aria-hidden="true" class="w-6 text-center text-xl">
                  {item.icon}
                </span>
                <span
                  class={`whitespace-nowrap transition-opacity duration-150 ${
                    expanded() ? "opacity-100" : "opacity-0"
                  }`}
                  data-testid={`nav-label-${item.id}`}
                >
                  {t(item.labelKey)}
                </span>
              </button>
            )}
          </Focusable>
        )}
      </For>
    </nav>
  );
};
