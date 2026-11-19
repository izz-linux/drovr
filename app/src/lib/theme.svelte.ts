import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DEFAULT_THEME, THEME_SETTINGS, isThemeSetting, resolveTheme, themeById, type ThemeSetting } from "./themes";

const DARK_QUERY = "(prefers-color-scheme: dark)";

export const theme = $state({ setting: DEFAULT_THEME as ThemeSetting });

function show() {
  const dark = window.matchMedia?.(DARK_QUERY).matches ?? false;
  const t = themeById(resolveTheme(theme.setting, dark));
  const root = document.documentElement;
  for (const [key, value] of Object.entries(t.colors)) root.style.setProperty(`--${key}`, value);
  if (t.lettering) {
    root.style.setProperty("--title", t.lettering.color);
    root.style.setProperty("--title-fill", t.lettering.foil);
    root.dataset.foil = "";
  } else {
    for (const p of ["--title", "--title-fill"]) root.style.removeProperty(p);
    delete root.dataset.foil;
  }
  root.style.colorScheme = t.scheme;
  // Last: the terminal reads the variables when data-theme changes.
  root.dataset.theme = t.id;
}

export async function setTheme(next: ThemeSetting) {
  theme.setting = next;
  show();
  try {
    await invoke("save_theme", { theme: next });
  } catch {}
}

/** Load the saved theme, follow macOS light/dark while on "Match System",
 *  and answer View > Theme. Returns a cleanup function. */
export function initTheme(): () => void {
  show();
  invoke<string | null>("load_theme")
    .then((stored) => {
      theme.setting = isThemeSetting(stored) ? stored : DEFAULT_THEME;
      show();
    })
    .catch(() => {});

  const query = window.matchMedia?.(DARK_QUERY);
  const onSystem = () => theme.setting === "system" && show();
  query?.addEventListener("change", onSystem);

  const unlisten = THEME_SETTINGS.map(({ id }) => listen(`menu:menu-view-theme-${id}`, () => setTheme(id)));
  return () => {
    query?.removeEventListener("change", onSystem);
    unlisten.forEach((p) => p.then((off) => off()));
  };
}
