import type { Theme, ThemePalette, ThemePalettes } from "../types";

export const DEFAULT_UI_PALETTES: ThemePalettes = {
  light: { accent: "#1BD96A", button: "#1BD96A", canvas: "#F1F4F1", surface: "#FFFFFF", ink: "#202720" },
  dark: { accent: "#1BD96A", button: "#1BD96A", canvas: "#0E0F0E", surface: "#161817", ink: "#EEF0EE" },
  custom: { accent: "#1BD96A", button: "#1BD96A", canvas: "#0E0F0E", surface: "#161817", ink: "#EEF0EE" },
};

export function isLightPalette(palette: ThemePalette): boolean {
  const [red, green, blue] = [palette.canvas.slice(1, 3), palette.canvas.slice(3, 5), palette.canvas.slice(5, 7)]
    .map((channel) => Number.parseInt(channel, 16) / 255)
    .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return red * 0.2126 + green * 0.7152 + blue * 0.0722 > 0.5;
}

export function themeClassName(theme: Theme, palette: ThemePalette): string {
  return theme === "custom" && isLightPalette(palette) ? "theme-custom theme-light-base" : `theme-${theme}`;
}

export function parseUiPalettes(value: string | undefined): ThemePalettes {
  try {
    const saved = JSON.parse(value ?? "null") as Partial<Record<Theme, Partial<ThemePalette>>> | null;
    if (!saved) return DEFAULT_UI_PALETTES;

    const isHexColor = (color: unknown): color is string =>
      typeof color === "string" && /^#[0-9a-fA-F]{6}$/.test(color);
    const legacyColors: Record<string, string> = {
      "#557b52": "#1BD96A",
      "#397f91": "#1BD96A",
      "#8264a8": "#1BD96A",
      "#b87932": "#1BD96A",
      "#b45468": "#1BD96A",
      "#f2f3ed": "#F1F4F1",
      "#202720": "#0E0F0E",
      "#fbfcf8": "#FFFFFF",
      "#252d25": "#161817",
      "#20251f": "#202720",
      "#edf0e9": "#EEF0EE",
    };
    const previousLightDefaults: Record<keyof ThemePalette, string> = {
      accent: "#1bd96a",
      button: "#1bd96a",
      canvas: "#0e0f0e",
      surface: "#161817",
      ink: "#eef0ee",
    };
    const normalizeColor = (color: unknown, fallback: string, field: keyof ThemePalette, theme: Theme): string => {
      if (!isHexColor(color)) return fallback;
      if (theme === "light" && color.toLowerCase() === previousLightDefaults[field]) return fallback;
      return legacyColors[color.toLowerCase()] ?? color;
    };
    const normalize = (palette: Partial<ThemePalette> | undefined, fallback: ThemePalette, theme: Theme): ThemePalette => ({
      accent: normalizeColor(palette?.accent, fallback.accent, "accent", theme),
      button: normalizeColor(palette?.button, palette?.accent && isHexColor(palette.accent) ? normalizeColor(palette.accent, fallback.button, "accent", theme) : fallback.button, "button", theme),
      canvas: normalizeColor(palette?.canvas, fallback.canvas, "canvas", theme),
      surface: normalizeColor(palette?.surface, fallback.surface, "surface", theme),
      ink: normalizeColor(palette?.ink, fallback.ink, "ink", theme),
    });
    return {
      light: normalize(saved.light, DEFAULT_UI_PALETTES.light, "light"),
      dark: normalize(saved.dark, DEFAULT_UI_PALETTES.dark, "dark"),
      custom: normalize(saved.custom, DEFAULT_UI_PALETTES.custom, "custom"),
    };
  } catch {
    return DEFAULT_UI_PALETTES;
  }
}
