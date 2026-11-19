# Gold themes, with themes defined as data

## Why

The user wants richer themes now, gold lettering in particular, and wants to be able to add theme packs later, their own or other people's, without a new release. This release ships four gold themes and moves every theme into one data file. Next release, a theme pack is then a file in that same format, read from disk.

Mockups: https://claude.ai/artifact/Kxv5wVpHdDMCyXwS6oZ8AE

## Goals

- Add four dark themes: **Gilded**, **Deco Navy**, **Oxblood**, **Ink & Gold**, with the palettes and display faces in the mockups.
- Add *lettering* to the theme model: a title color, a foil gradient and a display face. Existing themes default to their text color and the system font, so they look exactly as they do today.
- Make `themes.json` the one source of truth for themes, read by both the UI and the Rust menu.

## Non-goals (this release)

- Loading theme packs from disk, Base16 import, or any UI for managing packs. Those come next release, in the same format.
- A foil/flat toggle in Settings. The gold themes use foil, and flat is the fallback where `background-clip: text` isn't supported.
- Changing the logo. `static/drovr.svg` is used as-is in every theme.

## Anti-goals

- Gold on running text. Agent output, markdown bodies and the composer stay on the system font and theme text color. Gold only goes on short labels.
- Any visible change to the five existing themes.
- Fonts loaded from the network. The CSP allows `font-src 'self' data:` only.

## Approaches

- **A. Themes as data (chosen).** `themes.json` holds every theme. The UI sets CSS variables from it, and Rust `include_str!`s it to build View → Theme. Packs later just merge more entries in.
- **B. More CSS blocks.** It's quicker, but the five hard-coded lists stay, and next release has to do A anyway before packs can work.
- (Considered as the pack format: Base16 maps well onto the colors but has no lettering, and VS Code theme JSON has hundreds of keys we'd ignore. The drovr format stays its own, and a Base16 import is planned for the pack release.)

## Design

### `app/src/lib/themes.json`

```jsonc
[
  { "id": "dark", "name": "Dark", "scheme": "dark",
    "colors": { "bg": "#1e1e1e", "panel": "…", "panel-2": "…", "border": "rgba(…)",
                "text": "…", "muted": "…", "accent": "…", "on-accent": "…",
                "green": "…", "yellow": "…", "red": "…", "purple": "…", "cyan": "…" } },
  { "id": "gilded", "name": "Gilded", "scheme": "dark",
    "colors": { … },
    "lettering": { "color": "#e6c768",
                   "foil": "linear-gradient(180deg,#f8e7a6 0%,#e2bf5a 45%,#a8802a 70%,#e9cd78 100%)",
                   "font": "Cinzel" } }
]
```

- The file is in display order: Light, Dark, Darcula, Solarized Dark, Nord, then the four gold themes. "Match System" isn't an entry; it's added in code as now.
- `scheme` sets `color-scheme` and tells the terminal and the system-resolution logic which kind of theme it is.
- `lettering` is optional. If it's missing, the title color is `--text`, there's no foil and the font is the UI font.
- Colors move out of `app.css`. `app.css` keeps one `:root` block with Dark's values so the first paint before JS runs looks as it does now ("Dark shows until the saved theme loads").

### UI (`themes.ts`, `theme.svelte.ts`)

- `themes.ts` imports the JSON and derives `ThemeId`, `THEMES` and `THEME_SETTINGS`. `ThemeId` becomes `string` checked against the list, since packs will add ids at runtime.
- `show()` keeps setting `data-theme` (the terminal's MutationObserver depends on it), and also writes each color to `--<key>` plus `--title`, `--title-fill`, `--display` and `color-scheme` on `document.documentElement.style`.
- Lettering CSS in `app.css`: `.title { color: var(--title); font-family: var(--display); }`, with the foil applied under `@supports (background-clip: text)` and only when `--title-fill` is set. That's done with a `data-foil` attribute that `show()` sets, since a CSS variable can't be tested for.
- Lettering goes on: the sidebar wordmark, the agent detail title, ★ lead agent names in the list, and the RUN labels in the feed. That's it.

### Rust (`menu.rs`)

- Delete the `THEMES` const. Build View → Theme from `include_str!("../../src/lib/themes.json")` parsed with `serde_json` (already a dependency), with "Match System" first and then a separator, as now.
- Replace the `themes_match_the_ui` text-parsing test with tests on the JSON itself (see Tests).

### Fonts

- Vendor Cinzel, Poiret One, Cormorant Garamond and Marcellus as Latin-subset `.woff2` (only the weights used) in `app/static/fonts/`, each with its `OFL.txt`. Declare them with `@font-face` in `app.css`. No npm dependencies.

### Docs

- README "Themes" section: list the new themes, drop "the same set as local-wiki" (the first five still match), and describe `themes.json` and the lettering keys.
- README architecture line: themes now live in `app/src/lib/themes.json`.
- The `app.css` header comment and the CLAUDE.md "Theme lists" convention: change both to say themes are in `themes.json` only.

## Tests

There's no frontend test runner, and adding one is out of scope, so theme rules are tested in Rust against `themes.json`:

- Ids are unique, non-empty and slug-safe (they become menu ids and `data-theme` values), and no theme is called `system`.
- Every theme has all 13 color keys, and each value is a hex or `rgba()` color.
- `scheme` is `light` or `dark`. If `lettering` is present, its `font` names a face declared in `app.css`.
- The menu built from the file has "Match System" plus one item per theme, in file order.
- Contrast: `text` on `bg` is at least WCAG AA 4.5:1 for every theme, which also guards packs later. `on-accent` on `accent` isn't checked, because existing Dark (white on `#4ea1f5`, about 2.8:1) would fail and its look must not change.

`pnpm check` must pass. `cargo test -p drovr-app` and clippy must stay green.

## Left to the builder

- Exact font weights and the subsetting tool.
- Whether the lead-agent star also takes the gold, or only the name.
