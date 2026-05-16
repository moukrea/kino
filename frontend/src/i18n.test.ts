import { describe, expect, it } from "vitest";

import { pickLocale } from "./i18n";

describe("pickLocale", () => {
  it("returns 'en' when the candidate list is undefined", () => {
    expect(pickLocale(undefined)).toBe("en");
  });

  it("returns 'en' when no candidate matches a supported locale", () => {
    expect(pickLocale(["zh-CN", "ja", "de"])).toBe("en");
  });

  it("matches the first supported language tag in the candidate list", () => {
    expect(pickLocale(["es", "fr-FR", "en-US"])).toBe("fr");
  });

  it("ignores region subtags when matching", () => {
    expect(pickLocale(["en-GB"])).toBe("en");
    expect(pickLocale(["fr-CA"])).toBe("fr");
  });

  it("is case-insensitive", () => {
    expect(pickLocale(["FR-fr"])).toBe("fr");
  });
});
