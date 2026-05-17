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

/**
 * Dynamic-key alias used by routes (F-016 Settings) that build i18n keys
 * at runtime from data — section ids, control ids, etc. — and can't carry
 * a literal-typed key through. Returns the resolved string, never a
 * sub-tree object: any nested sub-object lookup is coerced to its name.
 *
 * Prefer the typed `t(...)` everywhere a literal works; reach for `tDyn`
 * only at the dispatch boundary.
 */
export function tDyn(
  key: string,
  params?: Readonly<Record<string, string>>,
): string {
  // The translator's literal-typed signature rejects `string`. Casting via
  // `unknown` keeps the production call site type-stable while letting
  // the routes hand a runtime-shaped key down.
  const fn = t as unknown as (k: string, p?: Record<string, string>) => unknown;
  const result = fn(key, params);
  if (typeof result === "string") return result;
  // Sub-tree hit: stringify the keypath rather than render `[object Object]`,
  // so a misconfigured key surfaces clearly in QA instead of silently breaking
  // the UI.
  return key;
}
