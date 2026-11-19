# PR Reviewer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Notice PRs posted in configured Slack channels, or requesting a configured GitHub team's review, and offer them in the Inbox. On Dispatch, start a read-only reviewer agent in its own worktree of the PR, running the review skill on the review model. Clean it up when the human removes it, or when the PR closes.

**Architecture:**
- A new core module, `pr_review.rs`, finds and dedupes PRs and makes Inbox items (kind `review`).
- `Engine::dispatch_review(url)` prepares the clone and the PR worktree, then starts a solo agent with role `pr-reviewer`.
- `Engine::remove` force-cleans a `pr-reviewer`'s worktree and branch.
- The app's background loop calls `pr_review::poll`. The CLI gets `drovr review <url>` and `drovr review poll`.

**Tech Stack:** Rust (`drovr-core`, `drovr-cli`, Tauri `drovr-app`), Svelte 5 (an Inbox label), pnpm.

**Spec:** `docs/superpowers/specs/2026-11-18-pr-reviewer-design.md`

## Global Constraints

- **Config `[review]`** (struct `Review` in config.rs, field `review` on `Config`):

  | Key | Type | Default |
  |---|---|---|
  | `slack_channels` | `Vec<String>` | `[]` |
  | `github_teams` | `Vec<String>` | `[]` |
  | `poll_secs` | `u64` | `600` |
  | `auto_dispatch` | `bool` | `false` |
  | `remove_when_closed` | `bool` | `true` |
  | `repos_dir` | `String` | `""` |

  Nothing runs unless at least one channel or team is set and `poll_secs > 0`. `repos_dir` empty means `cfg.default_cwd()`.
- **Reused config:** the review model is `stages.models["review"]`, else empty (the default model). The skill is `stages.review_skill`. Both are `~`-expanded where they're paths.
- **The `gh` binary** is `cfg.watch.gh`. Tests fake it with a script, as `watch.rs` tests do (~:614).
- **PR URL form:** `https://github.com/<owner>/<repo>/pull/<n>`. The key is exactly that, without trailing segments or query.
- **State file:** `<home>/review.json`, `{ "since": <unix secs>, "seen": { "<url>": <unix secs> } }`. Drop `seen` entries older than 90 days on save. A corrupt or missing file means `{since: now - 86400, seen: {}}`.
- **Inbox item:**
  - kind `review`, agent `drovr`
  - title `Review <owner>/<repo>#<n>: <title>`
  - options `["Dispatch reviewer", "Dismiss"]`
  - payload `{ "url", "owner", "repo", "number", "title", "author", "source" }`, where `source` is `"#channel"` or `"team org/slug"`
  - the body has the URL, the author and the source, plus the Slack permalink and poster when known
- **The reviewer agent:**
  - name `review-<slug(repo)>-<n>`, made unique with `-2`, `-3` and so on if taken; role `pr-reviewer`; no group; the PR URL stored in its `ticket` field
  - its worktree is `<home>/worktrees/<name>` on branch `drovr/<name>`, created from `FETCH_HEAD` after `git fetch origin pull/<n>/head` in the clone `<repos_dir>/<repo>`. If the clone is missing, run `<gh> repo clone <owner>/<repo> <clone>` first.
- **Labels:** the Inbox kind label is "PR review", in both `Inbox.svelte` `kinds` and `remote.rs` `kind_label`.
- No new dependencies. Use string parsing, not regex, unless `regex` is already a dependency of drovr-core (check Cargo.toml).
- Commits are single-line Conventional Commits with no trailers. Run fmt, tests, clippy and `pnpm check` as separate commands. pnpm only.
- README.md, USAGE.md and config.example.toml are updated in this PR (Task 3).

## Review Focus

1. **The reviewer never writes to GitHub unprompted.** The task prompt says so plainly, and nothing in drovr posts.
2. **Each PR is offered once.** The same URL from Slack and from a team on the same poll gives one item. (Task 1 test.)
3. **Dispatch failure:**
   - a failed clone or fetch gives no agent
   - the item stays pending, with the error returned to the caller
   - no half-made worktree is left behind (Task 2 test)
4. **Removal:**
   - removing a `pr-reviewer` deletes its worktree, even when it's dirty, and its branch
   - removing any other agent behaves exactly as before (Task 2 test)
5. **Auto-removal** only touches idle `pr-reviewer`s whose PR is MERGED or CLOSED. (Task 2 test.)
6. **A poll failure in one source** doesn't stop the other, and never panics the app loop.

---

### Task 1: Find PRs and offer them

**Files:**
- Create: `crates/drovr-core/src/pr_review.rs` (with `mod tests`), and register it in `lib.rs`. (`review.rs` already exists for agent diff reviews; don't touch it.)
- Modify: `crates/drovr-core/src/config.rs`: add `Review` with `Default` and a `review` field on `Config` (follow how `Watch` is declared, ~:135).
- Modify: `crates/drovr-core/src/sources.rs`: add `pub fn slack_pr_messages(&self, channels: &[String], since: f64) -> Result<Vec<SlackMessage>>`.
  - `rest`/`command`: one `slack_search` per channel with the query `in:<channel> github.com after:<YYYY-MM-DD of since minus 1 day>`. The search is day-granular; dedupe handles the overlap.
  - `agent`: one `ask_agent` prompt asking for messages in those channels since that date that contain GitHub pull request links, returning a JSON array shaped like `SLACK_SHAPE`.
- Modify: `app/src/lib/components/Inbox.svelte` `kinds` and `crates/drovr-core/src/remote.rs` `kind_label`: add `review: "PR review"`.

**Interfaces:**
- Produces:
  - `pub fn pr_urls(text: &str) -> Vec<String>`, normalized and deduped in order
  - `pub struct Found { pub url, owner, repo, number: u64, title, author, source, permalink, poster }`
  - `pub fn poll(engine: &Engine, force: bool) -> Vec<Found>`, the newly offered PRs. It uses `poll_at(engine, force, &path)` for tests.
  - Task 2 hooks in `fn offer(engine, &Found)`: Task 1 writes the Inbox item. Task 2 adds the auto_dispatch branch and the closed-PR cleanup at the end of `poll_at`.

- [ ] **Step 1: Write the failing tests.**
  - `pr_urls`:
    - plain text
    - Slack `<https://github.com/a/b/pull/3|a/b#3>`
    - `/pull/12/files` and `?x=1` normalize to `/pull/12`
    - two links give two URLs
    - `/issues/4` and `/a/b` are ignored
  - `poll_at`, with a fake `gh` script that answers:
    - `search prs …` → JSON
    - `pr view <url> --json …` → per-URL JSON
    - `api user --jq .login` → `me`
  - The Slack fake is the `command` provider with `search_cmd` set to a script printing messages.
  - Cases:
    - a team PR gives one `review` item with the exact title, options and payload
    - the same URL in Slack and team results gives one item
    - a second poll gives no new item
    - a draft, a CLOSED PR and an author `me` PR give no item
    - Slack failing (the script exits 1) still gives the team's item
    - `poll_secs = 0` or no channels or teams gives nothing, and no `gh` call (the fake logs calls to a file)
    - not due yet (`since` recent, `force = false`) gives nothing
  - State: entries older than 90 days are dropped on save, and corrupt JSON means a fresh state.

- [ ] **Step 2: Run them and confirm they fail.** `cargo test -p drovr-core pr_review::`

- [ ] **Step 3: Implement** following the Global Constraints. For the due check, keep `last_poll` in state, and poll when `now - last_poll >= poll_secs` or when forced.

- [ ] **Step 4: Run the tests and confirm they pass.** `cargo test -p drovr-core pr_review::`, then `cargo test -p drovr-core`.

- [ ] **Step 5: Run the checks.** As separate commands: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cd app && pnpm check`.

- [ ] **Step 6: Commit.** `git commit -m "feat: offer PRs from Slack channels and team review requests"`

---

### Task 2: Dispatch, read-only task, and cleanup

**Files:**
- Modify: `crates/drovr-core/src/engine.rs`:
  - `pub fn dispatch_review(&self, url: &str) -> Result<Agent>`
  - `answer()` gets a `"review"` arm (pattern: `"watch"` ~:1891):
    - "Dispatch reviewer": `dispatch_review`, then resolve the item `answered` with `Reviewer <name> started`
    - "Dismiss": resolve `dismissed`
    - on a dispatch error: return Err and leave the item pending
  - `start_agent` skips `make_worktree` when `spec.role == "pr-reviewer"` (~L+43, beside the retro/review exclusions)
  - `remove()`: for role `pr-reviewer` with cwd under `<home>/worktrees/<name>`, run `git worktree remove --force <dir>`, then `git branch -D drovr/<name>` and `git worktree prune` in the clone. Return "Its worktree and branch were removed."
  - A new `fn make_pr_worktree(&self, clone: &Path, name: &str, number: u64) -> Result<String>`. If `worktree add` fails after a successful fetch, remove any partial dir.
- Modify: `crates/drovr-core/src/prompts.rs`: `pub fn pr_review_task(url: &str, skill: &str) -> String`. It says:
  - review PR `<url>`; the worktree is already at its head commit
  - follow the skill at `<skill>`, or do a thorough code review if it's empty
  - **read-only on GitHub:** don't push, comment, approve, request changes or edit the PR
  - publish the findings with `publish_report` titled `Review <owner>/<repo>#<n>`
  - `send_message` to `human`: the verdict plus the top findings in a few lines
  - then end your turn and wait; if the human asks you to post the review, post exactly what they approve with `gh pr review` or `gh pr comment`
- Modify: `crates/drovr-core/src/pr_review.rs`:
  - `offer()` gets the `auto_dispatch` branch: dispatch, plus an item already resolved `answered` with `Reviewer <name> started`. On error, fall back to a normal pending item with the error in the body.
  - At the end of `poll_at`, when `remove_when_closed`: for each `pr-reviewer` agent with `run_state == "idle"`, `gh pr view <ticket> --json state`. If MERGED or CLOSED, `engine.remove(name)` and record nothing else.

**Interfaces:**
- Consumes: `Found`, `poll_at` and `offer` from Task 1.
- Produces: `Engine::dispatch_review`, `prompts::pr_review_task`.

- [ ] **Step 1: Write the failing tests.**
  - Build a real local "origin":
    - `git init --bare origin.git`
    - a work clone with one commit pushed
    - a second commit pushed as `refs/pull/7/head` (`git push origin HEAD:refs/pull/7/head`)
    - the "clone" at `<repos_dir>/<repo>` is `git clone origin.git <repos_dir>/repo`
  - The fake gh's `repo clone` does `git clone <origin> <dest>`, and `pr view --json state` prints a configured state.
  - `dispatch_review`:
    - creates the agent `review-repo-7`, role `pr-reviewer`, ticket = url
    - cwd = `<home>/worktrees/review-repo-7`, whose HEAD equals the PR commit
    - model = `stages.models.review`
    - its task contains the URL, the skill path and "read-only"
  - A missing clone: the fake gh clones it, and dispatch succeeds.
  - A fetch failure (PR 99 has no ref): Err, no agent, no worktree dir.
  - The `answer` arm: Dispatch starts an agent and resolves the item. Dismiss starts none. A dispatch error leaves the item pending.
  - `remove` of a `pr-reviewer` with an untracked file in its worktree: the dir and branch are gone. `remove` of a solo agent with a worktree and an untracked file: the dir is kept (existing behaviour).
  - Auto-removal: an idle reviewer on a MERGED PR is removed. A working one is kept, and so is one on an OPEN PR.
  - `auto_dispatch`: a found PR starts an agent and records an answered item.
  - Use `cfg.defaults.auto_wake = false` and similar to stop tests launching real runs. Look at how existing engine tests start agents without running them; if `start_agent` launches a run, find the existing test pattern (e.g. `orchestrator.auto_spawn = false`, the backend set to something inert) and follow it.

- [ ] **Step 2: Run them and confirm they fail.**

- [ ] **Step 3: Implement.**

- [ ] **Step 4: Run the tests and confirm they pass.** Then run the full `cargo test -p drovr-core`.

- [ ] **Step 5: Run the checks.** fmt, clippy and `pnpm check`, as separate commands.

- [ ] **Step 6: Commit.** `git commit -m "feat: dispatch read-only PR reviewers in their own worktrees"`

---

### Task 3: Run it from the app and the CLI, plus the docs

**Files:**
- Modify: `app/src-tauri/src/lib.rs`: in the 15-second watcher thread, call `drovr_core::pr_review::poll(&e, false)` outside the `workflow.enabled` check, beside cleanup.
- Modify: `crates/drovr-cli/src/main.rs`:
  - `drovr review <pr-url>` dispatches and prints `started <name> in <cwd>`
  - `drovr review poll` runs `poll(engine, true)` and prints `offered <owner>/<repo>#<n>: <title> (<source>)` per PR, or `nothing new`
  - anything else prints the usage and exits 1
  - add both forms to `USAGE`
- Docs:
  - README:
    - a "Review PRs from Slack and GitHub" section
    - the `[review]` config rows
    - a line noting reviewers' worktrees are removed with them
  - USAGE:
    - a workflow section "### Review PRs posted in Slack or requested from your team": the config, the Inbox flow, telling the reviewer to post, removal and auto-removal, and the cost note (with the `agent` Slack provider each poll is a short `claude -p` run)
    - CLI reference entries
  - config.example.toml: a commented `[review]` block

- [ ] **Step 1: Write the failing test.** If you can factor the CLI parsing into a pure function, `review_args(&[String]) -> Result<ReviewCmd>`, test it: a URL, `poll`, and a bad arg. Otherwise note it in the report.

- [ ] **Step 2: Implement.**

- [ ] **Step 3: Verify.** As separate commands:
  - `cd app && pnpm build`
  - `cd app && pnpm cli`
  - `cd app && pnpm check`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - a manual `drovr review poll` with a temp `DROVR_HOME` and `DROVR_CONFIG` whose `[review]` has no channels or teams: it prints `nothing new`, makes no network calls and never touches real data

- [ ] **Step 4: Commit.** `git commit -m "feat: poll for PRs to review from the app and the CLI"`
