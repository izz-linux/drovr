// The same themes as local-wiki. The palettes are in app.css.
export type ThemeId = "light" | "dark" | "darcula" | "solarized-dark" | "nord";

export const THEMES: readonly { id: ThemeId; name: string }[] = [
  { id: "light", name: "Light" },
  { id: "dark", name: "Dark" },
  { id: "darcula", name: "Darcula" },
  { id: "solarized-dark", name: "Solarized Dark" },
  { id: "nord", name: "Nord" },
];

/** What's chosen: a theme, or "system" for Light or Dark following macOS. */
export type ThemeSetting = ThemeId | "system";

// Also listed in src-tauri/src/lib.rs for View > Theme; a Rust test checks they match.
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
