# PRs view

## Why

PR review offers currently land in the Inbox as one-off items. On the first poll that was 96 of them, mixed in with agents' questions. Once answered, they vanish, even though the PR and its reviewer live on until it merges. The human wants a **PRs** view: every PR from the `[review]` sources that's waiting on them, from offer to merge, in one place.

## Goals

1. **A PRs view** (sidebar, ⌘8) lists PRs found by `pr_review`: Slack channels, team review requests, filtered by `owners`, never the human's own PRs. They're in two groups:
   - **New:** waiting on the human to decide. Actions: **Dispatch reviewer**, **Dismiss**, **Open on GitHub**.
   - **Reviewing:** a reviewer was dispatched. Actions:
     - **Open reviewer**, which goes to the agent in Agents, where the human can tell it to post
     - **Report**, which opens the reviewer's report "Review owner/repo#n" when one exists
     - **Open on GitHub**
     - **Dismiss**, which removes the reviewer and its worktree and hides the PR

   Each row shows:
   - `owner/repo#n`, the title, the author, the source (`#channel`, or `team org/slug`) and its age
   - CI (✓ ✗ ● or none)
   - GitHub's review decision, and the human's own latest review (waiting on you, you approved, you requested changes, you commented)
   - for Reviewing rows, the reviewer's state (working, idle or reported, error)
2. **Lifecycle,** stored in drovr's database:
   - `new`: from the poll, or `dispatched` straight away with `auto_dispatch` or `drovr review <url>`
   - `new` becomes `dispatched` on Dispatch
   - `new` or `dispatched` becomes `dismissed` on Dismiss. A dispatched PR's reviewer is removed: worktree, branch and folder, as removal does today.
   - `new` or `dispatched` becomes `closed` when the poll sees the PR MERGED or CLOSED. A dispatched PR's reviewer is removed when `review.remove_when_closed`, which is the existing behaviour.
   - Only `new` and `dispatched` rows show. `dismissed` and `closed` rows are kept for dedupe and pruned after 90 days, like `review.json`'s `seen`.
3. **Notifications and badges:**
   - New `new` rows raise a macOS notification while the app isn't focused: "PR to review: owner/repo#n" or "N PRs to review". Same rules as the Inbox's.
   - The PRs nav item shows a badge with the `new` count. The Dock badge is the Inbox count plus the `new` count.
4. **No more PR items in the Inbox:**
   - The poll stops creating `review` Inbox items.
   - On upgrade, pending `review` items are moved into the table as `new` rows and resolved with "Moved to PRs".
   - The `review` answer arm in `Engine::answer` stays, so old items still work if any remain.
5. **Live state:** each poll (`review.poll_secs`) refreshes every `new` and `dispatched` row with one `gh pr view` per PR: state, isDraft, CI rollup, reviewDecision, latestReviews. The human's own review comes from `latestReviews` matched to the gh login, which `pr_review` already caches per poll. The view also has a **Refresh** button that runs a forced poll.
6. **Keyboard:**
   - ⌘8 is PRs. Settings moves to ⌘, and gets a **Settings…** item in the drovr app menu with that shortcut.
   - The PRs nav item shows only when `[review]` has a channel or a team, or rows exist. Otherwise ⌘8 does nothing.
   - The shortcuts sheet and USAGE are updated.
7. **CLI:**
   - `drovr review list` prints the open rows: `new|reviewing  owner/repo#n  title  (source)  reviewer`.
   - `drovr review dismiss <url>` dismisses one.
   - `drovr review <url>` also records the PR as `dispatched`.

## Non-goals

- PRs authored by the human. Pipeline covers those.
- Approving or merging from drovr.
- The phone page. It doesn't show PR rows yet, so a note in the docs says so.

## Design

- **Store:** a new table, `review_prs`:

  | Column | Type | Notes |
  |---|---|---|
  | `key` | TEXT PRIMARY KEY | lowercased canonical URL |
  | `url`, `owner`, `repo`, `title`, `author`, `source`, `permalink`, `poster` | TEXT | |
  | `number` | INTEGER | |
  | `status` | TEXT | `new` / `dispatched` / `dismissed` / `closed` |
  | `agent` | TEXT | |
  | `offered_ts`, `decided_ts`, `checked_ts` | REAL | |
  | `pr_state`, `ci`, `review_decision`, `my_review` | TEXT | |
  | `draft` | INTEGER | |

  Store methods: `upsert_review_pr`, `review_prs(open_only)`, `set_review_pr_status(key, status, agent)`, `update_review_pr_live(key, …)`, `prune_review_prs(before)`. The migration creates the table and moves pending `review` items.
- **`pr_review.rs`:**
  - `offer()` writes a row instead of an Inbox item. With `auto_dispatch`, it dispatches and writes the row as `dispatched`.
  - A new `refresh_open(engine)` runs on each due poll, after offering. It covers the `closed` transition and reviewer removal, and replaces `remove_closed` for tracked PRs. `remove_closed` stays for reviewers that have no row, for example ones started before the upgrade.
- **Engine:** `dispatch_pr(key)` (wraps `dispatch_review`, then sets `dispatched` plus the agent) and `dismiss_pr(key)` (removes the reviewer if there is one, then sets `dismissed`). `dispatch_review` callers that aren't through a row (the CLI) upsert the row too.
- **App:**
  - `Snapshot` gains `review_prs: Vec<ReviewPr>` (the open rows).
  - New Tauri commands: `pr_dispatch(key)`, `pr_dismiss(key)` and `pr_refresh()` (a forced poll).
  - A new `Prs.svelte` view.
  - `notify.ts` gains `prsChanged(newRows, notify)`, and the Dock badge combines both counts.

## Tests

- **Store:** upsert keeps the status and the agent on later upserts; only open rows are listed; prune.
- **Migration:** a pending `review` item becomes a `new` row and the item is resolved; already-answered items are left alone.
- **Poll:** a found PR makes a `new` row and no Inbox item; dedupe still holds; `auto_dispatch` makes a `dispatched` row with an agent.
- **Refresh:**
  - live fields update, and `my_review` is read from `latestReviews` for the login
  - MERGED makes the row `closed` and removes the reviewer (with `remove_when_closed`); with it false, the row is `closed` and the reviewer kept
  - a `new` row on a CLOSED PR becomes `closed` with no agent
- **Engine:** `dispatch_pr` sets the status and agent, and a failure leaves the row `new`; `dismiss_pr` removes the reviewer and sets `dismissed`.
- **CLI:** argument parsing for `list` and `dismiss`.
