import themes from "./themes.json";

export type Theme = {
  id: string;
  name: string;
  scheme: "light" | "dark";
  colors: Record<string, string>;
  lettering?: { color: string; foil: string };
};

// Defined once in themes.json, which src-tauri/src/themes.rs also reads for View > Theme.
export const THEMES: readonly Theme[] = themes as Theme[];

export type ThemeId = string;

/** What's chosen: a theme, or "system" for Light or Dark following macOS. */
export type ThemeSetting = ThemeId | "system";

export const THEME_SETTINGS: readonly { id: ThemeSetting; name: string }[] = [
  { id: "system", name: "Match System" },
  ...THEMES,
];

export const DEFAULT_THEME: ThemeSetting = "system";

export function isThemeSetting(v: unknown): v is ThemeSetting {
  return v === "system" || THEMES.some((t) => t.id === v);
}

/** The theme to show for a setting, given whether the system is in dark mode. */
export function resolveTheme(setting: ThemeSetting, systemDark: boolean): ThemeId {
  if (setting !== "system") return setting;
  return systemDark ? "dark" : "light";
}

/** A theme by id, or Dark if it's unknown. */
export function themeById(id: string): Theme {
  return THEMES.find((t) => t.id === id) ?? THEMES.find((t) => t.id === "dark")!;
}
