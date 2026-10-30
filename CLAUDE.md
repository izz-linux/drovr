# CLAUDE.md — drovr

## Rules

- **Always use pnpm, never npm or npx.** Install with `pnpm install`, run scripts with `pnpm <script>`, and run tools with `pnpm exec` or `pnpm dlx`. Don't add a `package-lock.json`.
- **Pin every GitHub Action to a full commit SHA, never a floating tag or branch** (`@v4`, `@main`, `@stable`). Put the version in a comment:

  ```yaml
  - uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5.1.0
  ```

  Resolve the SHA with `git ls-remote --tags https://github.com/<owner>/<repo>`, and use the peeled `^{}` commit for annotated tags. If an action takes its behavior from the ref name, as `dtolnay/rust-toolchain` does, pass the setting as an input (`toolchain: stable`).

- **Commit as the owner, with no trailers.** Author and commit as `Izz Noland <izz@linux.com>` (`git config user.name "Izz Noland"` and `git config user.email "izz@linux.com"` in the clone). Don't add `Co-Authored-By`, `Claude-Session` or any other trailer to commit messages, and don't end PR descriptions with a "Generated with Claude Code" line or a session link.

- **Write commit messages and PR titles as Conventional Commits** (`fix: …`, `feat: …`, `docs: …`, `feat!: …` for breaking changes). release-please reads them from `main` to pick the next version and write the changelog, so don't bump versions by hand.
- **Keep `README.md` and `USAGE.md` up to date with the code.** Any change to behavior, CLI commands, configuration keys, defaults or the UI updates them in the same pull request. `README.md` covers what drovr is, setup, configuration and architecture; `USAGE.md` covers keyboard shortcuts, workflows and the full CLI reference.

## Stack

- **App:** Tauri 2 (Rust) with a Svelte 5 + SvelteKit (static) frontend in `app/`
- **Core:** `crates/drovr-core`: SQLite state, the agent engine, the coordination MCP server, Jira/Confluence/Slack lookups, reports
- **CLI:** `crates/drovr-cli`: the `drovr` binary. It's also each agent's MCP server and the hook headless runs call when they finish. It's bundled into the app as a sidecar.
- **Package manager:** pnpm (`packageManager` in `app/package.json`)
- **Target:** macOS (Apple Silicon). CI: `.github/workflows/ci.yml` runs checks on PRs (no bundle); `.github/workflows/release-please.yml` keeps a release PR on `main`; merging it tags the release and runs `.github/workflows/release.yml`, which builds a signed, notarized `.dmg` (a hand-pushed `v*` tag also works).

## Commands

```bash
cd app && pnpm install
cd app && pnpm check                     # svelte-check / TypeScript
cd app && pnpm tauri dev                 # run the app
cd app && pnpm tauri build --bundles app dmg   # unsigned .app + .dmg
cd app && pnpm tauri:build:signed        # signed + notarized, see docs/signing.md
cargo test -p drovr-core                 # core tests
cargo test -p drovr-app                  # app tests (needs app/build: build the frontend first)
cargo clippy --workspace --all-targets -- -D warnings   # lint, as CI does (same prerequisite)
```

## Conventions

- **Theme lists:** themes are listed in both `app/src/lib/themes.ts` and `app/src-tauri/src/menu.rs` (for View → Theme). A test checks they match, so change both together. Palettes are in `app/src/app.css` and match local-wiki's.
- **Harnesses:** anything that depends on one agent CLI (its flags, its JSON output, its costs, its saved sessions) goes behind the `Harness` trait in `crates/drovr-core/src/harness/`, never in the engine. A new CLI is a new file there that implements the trait, plus an entry in `all()`.
- **Reports:** reports are `.html` files under `~/.claude/plans/`. A group's go in `plans/<group>/`, and a solo agent's in the root.
- **herdr:** it's driven only through the command templates in `[herdr]` in the config. macOS rejects typed input lines over about 1 KB, so long prompts and commands go through a file.
- **Signing:** the signing identity is never put in `tauri.conf.json`. It comes from a git-ignored `.env.signing`, the same as local-wiki.
