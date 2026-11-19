# Watcher Triggers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wake the owner when an open PR's CI turns green, tell the human when a PR is green and waiting on review, fire `new_review` for every review, and stop dropping merged and closed PRs from the watcher's state.

**Architecture:** There are two new triggers in `watch.rules`, which `rules::on_pr` evaluates with the existing `fired` dedupe and `act()`. The `new_review` dedupe key includes the review count. `watch::poll_prs` keeps known PRs of any state.

**Tech Stack:** Rust (`drovr-core`), serde, the existing `gh` CLI calls.

**Spec:** `docs/superpowers/specs/2026-11-18-watch-triggers-design.md`

## Global Constraints

- Trigger names are `ci_passed` (default `"wake"`) and `ready_for_review` (default `"ask"`).
- `ready_for_review` fires once per head (key `ready_for_review@<head>`) whenever the conditions hold, not only on a transition.
- `new_review`'s key is `new_review@<head>#<reviews>`.
- `ci_passed` doesn't fire when `before.ci` is empty (a PR seen for the first time).
- No new dependencies. Commits are single-line Conventional Commits with no body and no trailers. Run fmt, tests and clippy as separate commands.
- README.md and USAGE.md are updated in the same PR.

## Review Focus

1. **A draft PR** that's green and mergeable must not produce `ready_for_review`. (Task 1 test.)
2. **CI going `failure → success`** on the same head that already fired `ci_failed` must still fire `ci_passed`. The keys differ by trigger name. (Task 1 test.)
3. **An approved PR** goes through `approved_green` only, never `ready_for_review` as well. (Task 1 test.)
4. **A merged PR missing from one poll's search** must keep its `fired` list, and mustn't produce another `pr_linked` record when it reappears. (Task 2 test.)
5. **A known closed PR whose ticket left the JQL** is dropped, as before. Nothing should keep PRs alive for unwatched tickets. (Task 2 test.)

---

### Task 1: `ci_passed`, `ready_for_review` and per-review `new_review`

**Files:**
- Modify: `crates/drovr-core/src/config.rs:160-205` (`Rules` struct, `Default`, `action`)
- Modify: `crates/drovr-core/src/rules.rs:47-100` (`on_pr`), `:153-164` (`headline`), `:233-…` (`pr_text`)
- Test: `crates/drovr-core/src/rules.rs` `mod tests` (reuse `setup` and `pr(ci, review, mergeable, state)`)
- Docs: `README.md` (the trigger table near 362-372, the config row at 667), `USAGE.md:111` and `:285`

**Interfaces:**
- Produces: `Rules { ci_passed: String, ready_for_review: String, .. }`, and `Rules::action("ci_passed" | "ready_for_review")`.

- [ ] **Step 1: Write the failing tests** in `rules.rs` tests. Set `engine.cfg.watch.rules` actions to `"ask"` where you need Inbox items to observe, and assert on `now.fired`.
  - `ci_passed_fires_when_checks_turn_green`: `pr("pending",…,"OPEN")` → `pr("success",…)`, and `fired` contains `"ci_passed@a1"`. Same for `failure → success`.
  - `ci_passed_ignores_a_pr_seen_green_first`: `before = Pr { state: "OPEN", ..Pr::default() }` (ci `""`), now `success`, and `fired` has no `ci_passed@`.
  - `ready_for_review_fires_once_per_head`: `before == now == pr("success","REVIEW_REQUIRED","MERGEABLE","OPEN")` with empty `fired`. The first `on_pr` adds `"ready_for_review@a1"`, and a second call leaves exactly one Inbox item.
  - `ready_for_review_needs_green_mergeable_and_no_decision`: no key for each of draft (`draft: true`), `"CHANGES_REQUESTED"`, `"APPROVED"`, `"CONFLICTING"`, and ci `"failure"` or `"pending"`. With ci `"none"` (no checks), the key IS added.
  - `new_review_fires_for_each_review_on_a_head`: `before.reviews = 0` → `now.reviews = 1` adds `new_review@a1#1`, then `before.reviews = 1` → `now.reviews = 2` adds `new_review@a1#2`.
  - `default_rules_for_new_triggers`: `Rules::default().action("ci_passed") == "wake"` and `.action("ready_for_review") == "ask"`.

- [ ] **Step 2: Run them and confirm they fail.**
Run: `cargo test -p drovr-core rules::`
Expected: compile errors or FAIL on the new tests.

- [ ] **Step 3: Implement.**
  - Config: add the fields with doc comments in the style of the neighbors.
  - `on_pr`:
    - Push `ci_passed` when `pr.state == "OPEN" && pr.ci == "success" && matches!(before.ci.as_str(), "pending" | "failure")`.
    - Push `ready_for_review` when `pr.state == "OPEN" && !pr.draft && matches!(pr.ci.as_str(), "success" | "none") && pr.mergeable == "MERGEABLE" && !matches!(pr.review.as_str(), "APPROVED" | "CHANGES_REQUESTED")`.
    - Build the dedupe key as `format!("new_review@{}#{}", pr.head, pr.reviews)` for `new_review`. Keep `format!("{trigger}@{head}")` for everything else.
  - `headline`: `"ci_passed" => "CI passed"`, `"ready_for_review" => "green, waiting on review"`.
  - `pr_text`:
    - `ci_passed`: body is `head` plus "\nIts checks all passed.", and the to-do is `"Continue with what you were waiting on for this PR. If nothing is left, say so."`
    - `ready_for_review`: body is `head` plus "\nIts checks pass and it has no conflicts; it needs a review.", and there's no to-do.

- [ ] **Step 4: Run the tests and confirm they pass.**
Run: `cargo test -p drovr-core rules::`
Expected: PASS.

- [ ] **Step 5: Update the docs.**
  - README trigger table: add rows for `ci_passed` (wake, "Its checks went from pending or failing to passing", "CI passed and a nudge to carry on") and `ready_for_review` (ask, "Green, mergeable, no review decision yet; once per commit", "A 'waiting on review' item for you").
  - Change the `new_review` row to say it fires for each review.
  - Add the two names to the config row (667), USAGE:111's list and USAGE:285's trigger list.

- [ ] **Step 6: Run the full checks.**
Run, as separate commands: `cargo fmt --all`, then `cargo test -p drovr-core`, then `cargo clippy --workspace --all-targets -- -D warnings`.
Expected: all green.

- [ ] **Step 7: Commit.** `git commit -m "fix: wake owners on green CI and flag PRs waiting on review"`

---

### Task 2: Keep known PRs in the watcher's state

**Files:**
- Modify: `crates/drovr-core/src/watch.rs:354-366` (the loop that re-adds PRs the search missed) and the `look` condition at about `:371-381`
- Test: `crates/drovr-core/src/watch.rs` `mod tests` (the existing fake-`gh` poll tests at about `:575-715` show how to script `search.json` and `view.json`)
- Docs: README "Into the record" paragraph (342), only if it describes this behavior

**Interfaces:**
- Consumes: nothing from Task 1.

- [ ] **Step 1: Write the failing tests.**
  - `a_merged_pr_the_search_misses_keeps_its_history`:
    1. Poll once as a baseline, with a search returning an open PR for a watched ticket.
    2. Poll with it MERGED, so `fired` gains `merged@…` and the record has one `pr_merged`.
    3. Poll with a search that returns nothing.
    4. Poll with the search returning it again.

    Assert: the PR is still in `state.prs` after step 3 with its `fired` intact, and the ticket's record has exactly one `pr_linked` for it.
  - `a_pr_whose_ticket_left_the_jql_is_dropped`: a known CLOSED PR whose ticket is no longer in `state.tickets`, and a search that misses it. It's gone from `state.prs`.

- [ ] **Step 2: Run them and confirm they fail.**
Run: `cargo test -p drovr-core watch::`
Expected: the first test FAILs (the PR is dropped and relinked).

- [ ] **Step 3: Implement.** In the "search no longer returns" loop, drop the `old.state == "OPEN"` condition for keeping the PR. Insert it into `found` whenever one of its tickets is in `keys`. Add the URL to `unsearched` only when `old.state == "OPEN"`, so only open PRs are re-read. A kept non-open PR takes the non-`look` branch and keeps its `fired`.

- [ ] **Step 4: Run the tests and confirm they pass.**
Run: `cargo test -p drovr-core watch::`
Expected: PASS.

- [ ] **Step 5: Run the full checks.** As separate commands: `cargo fmt --all`, `cargo test -p drovr-core`, and `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 6: Commit.** `git commit -m "fix: keep merged and closed PRs in the watcher's state"`
