import { describe, expect, it } from "vitest";
import { DEFAULT_UI_PALETTES, parseUiPalettes } from "./palettes";

describe("parseUiPalettes", () => {
  it("falls back to defaults for invalid persisted JSON", () => {
    expect(parseUiPalettes("not-json")).toEqual(DEFAULT_UI_PALETTES);
  });

  it("normalizes missing and malformed colors while preserving valid colors", () => {
    const palettes = parseUiPalettes(JSON.stringify({
      light: { accent: "#123456", canvas: "invalid" },
    }));

    expect(palettes.light.accent).toBe("#123456");
    expect(palettes.light.canvas).toBe(DEFAULT_UI_PALETTES.light.canvas);
    expect(palettes.dark).toEqual(DEFAULT_UI_PALETTES.dark);
  });
});