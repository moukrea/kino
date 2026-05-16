// PRD §3 locks `@solid-primitives/i18n` (ADR-018) as the frontend i18n stack.
// PRD §5 "Internationalization": v1 ships English (`en`) and French (`fr`).
import { resolveTemplate, translator, flatten } from "@solid-primitives/i18n";
import { createMemo, createSignal } from "solid-js";

import en from "./locales/en.json";
import fr from "./locales/fr.json";

export type SupportedLocale = "en" | "fr";

export const SUPPORTED_LOCALES: readonly SupportedLocale[] = ["en", "fr"];

const DICTIONARIES = {
  en: flatten(en),
  fr: flatten(fr),
} as const;

export type Dictionary = (typeof DICTIONARIES)["en"];

/**
 * Pick the closest supported locale from a list of candidate language tags
 * (e.g. `navigator.languages`). Falls back to "en" when nothing matches.
 */
export function pickLocale(
  candidates: readonly string[] | undefined,
): SupportedLocale {
  if (!candidates) return "en";
  for (const candidate of candidates) {
    const tag = candidate.toLowerCase().split("-")[0];
    if (SUPPORTED_LOCALES.includes(tag as SupportedLocale)) {
      return tag as SupportedLocale;
    }
  }
  return "en";
}

function detectLocale(): SupportedLocale {
  if (typeof navigator === "undefined") return "en";
  return pickLocale(navigator.languages);
}

const [locale, setLocale] = createSignal<SupportedLocale>(detectLocale());

export { locale, setLocale };

// `translator` (from @solid-primitives/i18n) accepts the memo accessor itself
// and tracks the signal internally; eslint-plugin-solid can't see across the
// boundary so it warns about the accessor being passed without a tracked
// scope. The pattern is the library's documented usage.
const dictionary = createMemo(() => DICTIONARIES[locale()]);

// eslint-disable-next-line solid/reactivity
export const t = translator(dictionary, resolveTemplate);
