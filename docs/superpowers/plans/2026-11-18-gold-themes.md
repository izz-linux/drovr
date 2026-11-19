# Gold Themes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add four gold-lettering themes, and make one `themes.json` the source of every theme for both the UI and View → Theme, so theme packs can plug in next release.

**Architecture:** `app/src/lib/themes.json` lists every theme: its id, name, light/dark scheme, 13 colors and optional lettering. Rust `include_str!`s it in a new `themes.rs` module that the menu builds from, and that carries the validation tests. The UI imports the same file and writes the chosen theme's colors as inline CSS variables on `<html>`. The bundled display fonts are declared in `app.css`.

**Tech Stack:** Tauri 2 (Rust, serde_json), Svelte 5 / SvelteKit, TypeScript, pnpm.

**Spec:** `docs/superpowers/specs/2026-11-18-gold-themes-design.md`

## Global Constraints

- The five existing themes (Light, Dark, Darcula, Solarized Dark, Nord) keep exactly their current colors and look. They have no `lettering`.
- Menu order: Match System, separator, Light, Dark, Darcula, Solarized Dark, Nord, Gilded, Deco Navy, Oxblood, Ink & Gold.
- New ids: `gilded`, `deco`, `oxblood`, `ink`.
- Fonts load only from the app (CSP `font-src 'self' data:`). Vendored `.woff2` files with `OFL.txt`, and no new npm or cargo dependencies.
- Gold lettering goes on four places only: the sidebar wordmark, the agent detail `<h2>`, ★ lead-agent names in the agent list, and the run separators in the feed.
- pnpm only. Branch `feat/gold-themes`. **Don't commit or stage without the user's explicit OK.** Commit messages are a single-line Conventional Commit with no trailers.
- Run edits, fmt, tests and clippy as separate commands, not one chained command.
- README.md and CLAUDE.md are updated in the same PR.

## Review Focus

1. **Switching from a gold theme to one without lettering** must clear `--title`, `--title-fill`, `--display` and `data-foil`, so there's no leftover gold or serif on Dark. (Task 2, manual check step.)
2. **The terminal must recolor on a theme change.** Its MutationObserver fires on the `data-theme` change and reads the CSS variables at that moment, so `show()` must write the variables *before* it sets `data-theme`. (Task 2, Step 3 and manual check.)
3. **The first paint before JS runs** must look like today's Dark. `app.css` keeps Dark's values on `:root`. (Task 2, manual check.)
4. **A saved theme id that's no longer in the list** (a pack removed later) falls back to Match System in the UI, and no menu item is checked rather than a panic. (Task 1 test `unknown_saved_theme_checks_nothing`.)
5. **Long agent names in foil** still truncate with an ellipsis and stay readable when selected. (Task 2, manual check.)

---

### Task 1: `themes.json` and a Rust themes module the menu builds from

**Files:**
- Create: `app/src/lib/themes.json`
- Create: `app/src-tauri/src/themes.rs`
- Modify: `app/src-tauri/src/lib.rs:7` (add `mod themes;`)
- Modify: `app/src-tauri/src/menu.rs:14-22` (delete the `THEMES` const), `:139-155` (the theme submenu loop), and `:219` and `:279-298` (remove the `themes_match_the_ui` test and the `use super::THEMES`)

**Interfaces:**
- Produces (JSON): an array of `{ "id": string, "name": string, "scheme": "light"|"dark", "colors": { 13 keys }, "lettering"?: { "color": string, "foil": string, "font": string } }`. The 13 color keys are `bg panel panel-2 border text muted accent on-accent green yellow red purple cyan`.
- Produces (Rust, `themes.rs`):
  - `pub const COLOR_KEYS: [&str; 13]`
  - `#[derive(Deserialize)] pub struct Lettering { pub color: String, pub foil: String, pub font: String }`
  - `#[derive(Deserialize)] pub struct Theme { pub id: String, pub name: String, pub scheme: String, pub colors: BTreeMap<String, String>, pub lettering: Option<Lettering> }`
  - `pub fn builtin() -> Vec<Theme>`, which parses `include_str!("../../src/lib/themes.json")` and panics on bad JSON (it's compiled in, and the tests catch that)
  - `pub fn menu_entries() -> Vec<(String, String)>`, returning `("system", "Match System")` followed by `(id, name)` for each theme in file order
  - `fn contrast(a: &str, b: &str) -> f64`, the WCAG contrast ratio of two `#rrggbb` colors

- [ ] **Step 1: Write `themes.json`.** Copy the five existing palettes verbatim from `app/src/app.css`: `light` is lines 26-39, `dark` 9-22, `darcula` 44-57, `solarized-dark` 61-74, `nord` 78-91. Set `scheme` to `light` for Light and `dark` for the rest. Then add the four gold themes with these exact values:

| key | gilded | deco | oxblood | ink |
|---|---|---|---|---|
| name | Gilded | Deco Navy | Oxblood | Ink & Gold |
| bg | #15120d | #0e1621 | #1a1012 | #0b0b0b |
| panel | #100e0a | #0a1019 | #140c0e | #070707 |
| panel-2 | #1f1b14 | #16202e | #241619 | #151515 |
| border | rgba(212, 175, 55, 0.16) | rgba(201, 162, 75, 0.22) | rgba(230, 200, 140, 0.14) | rgba(255, 255, 255, 0.07) |
| text | #e9e2d0 | #e4e0d4 | #ece0d6 | #d9d6cf |
| muted | #9c907a | #8a93a3 | #a08f88 | #7e7b74 |
| accent | #d4af37 | #c9a24b | #e3c38a | #cfa646 |
| on-accent | #15120d | #0e1621 | #1a1012 | #0b0b0b |
| green | #9cc58a | #7fc4a4 | #a8c48c | #86c08f |
| yellow | #e8c66a | #e5c56f | #f0cf86 | #e2c060 |
| red | #e07a5f | #e3796b | #ff8a7a | #e8786a |
| purple | #c9a0dc | #b6a2e0 | #d0a3c8 | #bb9bd6 |
| cyan | #8fc7c0 | #7cc3d9 | #9cc9c4 | #7fbfc9 |
| lettering.color | #e6c768 | #d9b45e | #ecd29a | #d8b24e |
| lettering.foil | linear-gradient(180deg, #f8e7a6 0%, #e2bf5a 45%, #a8802a 70%, #e9cd78 100%) | linear-gradient(180deg, #f3dc9c 0%, #c9a24b 55%, #8e6a26 100%) | linear-gradient(180deg, #fff1cf 0%, #e3c38a 50%, #b08a4f 100%) | linear-gradient(180deg, #f1d985 0%, #cfa646 50%, #9a7428 100%) |
| lettering.font | Cinzel | Poiret One | Cormorant Garamond | Marcellus |

- [ ] **Step 2: Write the failing tests in `themes.rs` `mod tests`.**
  - `ids_are_unique_slugs`: every id matches `^[a-z0-9-]+$`, there are no duplicates, and none is `"system"`.
  - `every_theme_has_every_color`: for each theme, the `colors` keys equal `COLOR_KEYS` exactly, and each value starts with `#` (length 7) or `rgba(`.
  - `scheme_is_light_or_dark`.
  - `text_is_readable`: `contrast(colors["text"], colors["bg"]) >= 4.5` for every theme. The assert message names the theme.
  - `contrast_matches_known_values`: `contrast("#000000", "#ffffff")` is within 0.01 of 21.0, and `contrast("#777777", "#ffffff")` is within 0.05 of 4.48.
  - `existing_themes_keep_their_order`: the first six `menu_entries()` ids are `["system", "light", "dark", "darcula", "solarized-dark", "nord"]` and the last four are `["gilded", "deco", "oxblood", "ink"]`.
  - `existing_themes_have_no_lettering`: `light dark darcula solarized-dark nord` have `lettering == None`, and the four gold ones have `Some`.

- [ ] **Step 3: Run them and confirm they fail.**
Run: `cargo test -p drovr-app themes::`
Expected: a compile error or FAIL, because `builtin`, `menu_entries` and `contrast` don't exist yet. (This needs `app/build` to exist. If it doesn't, run `cd app && pnpm build` first.)

- [ ] **Step 4: Implement `themes.rs`.** Use serde_json for parsing. `contrast` uses WCAG relative luminance (sRGB channel `c/255`, then `c <= 0.03928 ? c/12.92 : ((c+0.055)/1.055)^2.4`, `L = 0.2126R + 0.7152G + 0.0722B`, ratio `(L1+0.05)/(L2+0.05)` with the lighter color on top). Add `mod themes;` to `lib.rs`.

- [ ] **Step 5: Point the menu at it.** In `menu.rs`, delete `THEMES`, and loop over `crate::themes::menu_entries()` in the theme submenu, keeping the separator after index 0. Remove `themes_match_the_ui` and its `use super::THEMES`. Add the test `unknown_saved_theme_checks_nothing`: no entry in `menu_entries()` has the id `"no-such-theme"`. This pins that the loop's `id == current` comparison simply leaves every item unchecked.

- [ ] **Step 6: Run the tests and confirm they pass.**
Run: `cargo test -p drovr-app`
Expected: PASS, including all of `themes::tests`.

- [ ] **Step 7: Run clippy.**
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Checkpoint.** Ask the user before committing. The proposed message is `feat: define themes in themes.json and add four gold themes`.

---

### Task 2: Apply themes from JSON in the UI, bundle the fonts, add the lettering, update the docs

**Files:**
- Create: `app/static/fonts/` with `cinzel-700.woff2`, `poiret-one-400.woff2`, `cormorant-garamond-700.woff2`, `marcellus-400.woff2`, and `OFL-cinzel.txt`, `OFL-poiret-one.txt`, `OFL-cormorant-garamond.txt`, `OFL-marcellus.txt`
- Modify: `app/src/lib/themes.ts` (whole file)
- Modify: `app/src/lib/theme.svelte.ts:9-12` (`show()`)
- Modify: `app/src/app.css:1-93` (palette blocks → one `:root` default, `@font-face`, lettering rules)
- Modify: `app/src/routes/+page.svelte:69` (wordmark)
- Modify: `app/src/lib/components/Agents.svelte:294` (★ name), `:312` (`<h2>`), `:356` (run `.sep`)
- Modify: `app/src-tauri/src/themes.rs` (one more test)
- Modify: `README.md:530-532` and `:824`, `CLAUDE.md` (the "Theme lists" convention)

**Interfaces:**
- Consumes: `themes.json` from Task 1.
- Produces (TS, `themes.ts`): `export type Theme = { id: string; name: string; scheme: "light" | "dark"; colors: Record<string, string>; lettering?: { color: string; foil: string; font: string } }`, `export const THEMES: readonly Theme[]`, `export type ThemeId = string`, `export type ThemeSetting = ThemeId | "system"`, `THEME_SETTINGS`, `DEFAULT_THEME`, `isThemeSetting` and `resolveTheme` (same signatures as now), and `export function themeById(id: string): Theme`, which falls back to the `dark` entry.
- Produces (CSS): the class `lettered` and the custom properties `--title`, `--title-fill`, `--display`, plus the attribute `data-foil` on `<html>`.

- [ ] **Step 1: Write the failing font test** in `themes.rs`: `lettering_fonts_are_bundled`. For each theme with lettering, `include_str!("../../src/app.css")` contains `font-family: "<font>"` inside an `@font-face`, and `app/static/fonts/` holds a `.woff2` whose name starts with the lowercased, hyphenated font name. Use `std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../static/fonts"))`.

- [ ] **Step 2: Run it and confirm it fails.**
Run: `cargo test -p drovr-app lettering_fonts_are_bundled`
Expected: FAIL (no `@font-face` and no fonts folder).

- [ ] **Step 3: Vendor the fonts.** Get the Latin `.woff2` files from Fontsource at a pinned version (`https://cdn.jsdelivr.net/fontsource/fonts/<slug>@<version>/latin-<weight>-normal.woff2`, with slugs `cinzel`, `poiret-one`, `cormorant-garamond`, `marcellus`), and each `OFL.txt` from `https://github.com/google/fonts/tree/main/ofl/<name>`. Echo each download command before running it.

- [ ] **Step 4: Rewrite `app.css` lines 1-93:**
  - Keep the `--mono` `:root` block.
  - Keep one `:root` block with Dark's 13 values and `color-scheme: dark` for the first paint. Delete the other five palette blocks. `[data-theme]` selectors are no longer used for colors.
  - Add `:root { --title: var(--text); --title-fill: none; --display: inherit; }`.
  - Add four `@font-face` blocks with `font-family: "Cinzel"`, etc., `src: url("/fonts/<file>.woff2") format("woff2")`, the matching `font-weight`, and `font-display: swap`.
  - Add `.lettered { color: var(--title); font-family: var(--display); font-synthesis: none; }`, and `@supports (background-clip: text) { [data-foil] .lettered { background: var(--title-fill); -webkit-background-clip: text; background-clip: text; color: transparent; } }`.
  - Rewrite the header comment: themes live in `src/lib/themes.json`. The first five match local-wiki. This file holds only Dark's first-paint values.

- [ ] **Step 5: Rewrite `themes.ts`** to import `./themes.json` and export the Interfaces above. Keep the comment pointing at the Rust side, updated to `src-tauri/src/themes.rs`.

- [ ] **Step 6: Rewrite `show()` in `theme.svelte.ts`.** Resolve the id, get the theme with `themeById`, and then **in this order**:
  1. Set each `--<key>` from `colors` on `document.documentElement.style`.
  2. Set `--title`, `--title-fill` and `--display` (`"<font>", Georgia, serif`) from `lettering`, or `removeProperty` each one when there's no lettering.
  3. Set `style.colorScheme = scheme`.
  4. Set or delete `dataset.foil`.
  5. Set `dataset.theme` **last**, since the terminal reads the variables when it changes.

- [ ] **Step 7: Mark the four places** with `class="lettered"`. In `+page.svelte:69`, wrap the text `drovr` in `<span class="lettered">` and leave the `<img>` alone. In `Agents.svelte:294`, add `lettered` to the name span only when `a.role === "orchestrator"`. Add it to the `<h2>` at `:312` and the run `.sep` at `:356` (that `.sep` is the run branch only).

- [ ] **Step 8: Run the font test and confirm it passes.**
Run: `cargo test -p drovr-app lettering_fonts_are_bundled`
Expected: PASS.

- [ ] **Step 9: Run the type check.**
Run: `cd app && pnpm check`
Expected: 0 errors.

- [ ] **Step 10: Run the full Rust tests.**
Run: `cargo test -p drovr-app`
Expected: PASS. `cd app && pnpm build` first if `app/build` is stale.

- [ ] **Step 11: Check it by hand in the app.** Run `cd app && pnpm tauri dev` and check:
  - The first paint is Dark.
  - Each of the 10 menu entries applies, and the menu checkmark follows it.
  - Going Gilded → Dark leaves no gold and no serif.
  - The terminal panel recolors on every switch.
  - A long ★ name in Oxblood truncates with an ellipsis.
  - Settings' theme dropdown lists the same 10 entries.

- [ ] **Step 12: Update the docs.**
  - README §Themes (530-532): list all nine themes plus Match System. Say the first five match local-wiki. Describe `app/src/lib/themes.json` with its fields (`id`, `name`, `scheme`, `colors`, optional `lettering` with `color`/`foil`/`font`) and say the lettering appears on the wordmark, agent titles, lead agents and run labels.
  - README :824: `app/src-tauri` themes → `themes.rs`, and add a line for `app/src/lib/themes.json`.
  - CLAUDE.md "Theme lists": themes are defined once in `app/src/lib/themes.json`. The UI and View → Theme both read it, `themes.rs` tests validate it, and a lettering font must be bundled in `app/static/fonts/` with an `@font-face` in `app.css`.

- [ ] **Step 13: Run clippy.**
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 14: Checkpoint.** Ask the user before committing. The proposed message is `feat: add gold lettering to themes`.
