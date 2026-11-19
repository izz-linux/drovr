# Watcher: wake on green CI, tell you when a PR waits on review

## Why

From the SRE-3311 and SRE-3462 retro, groups stalled with open PRs and nobody was woken. The watcher's state (`watch.json`) and the ticket record showed:

- Reviews and merges do wake the owner, as configured.
- PRs stall when CI goes green or the PR waits for a review. infrastructure#2108 and #2109 and stord-github#1446 are open with CI success and a review required, and nothing fired on them. No trigger covers either case. A lead that ends its turn "waiting on CI" sleeps until the human notices.
- `new_review` fires once per head commit (`new_review@<head>`), so a second review on the same commit is missed.
- Merged and closed PRs are dropped from the watcher's state whenever a search misses them, and rediscovered later. Each time, the record gets another `pr_linked` and the PR loses its `fired` history. #2098 was linked four times after it merged.

The retro proposed a 30-minute stall timer. That's rejected: it fires on groups that are quiet for good reasons and is late by design. These changes cover the observed cases directly.

## Goals

1. **`ci_passed`:** an open PR's checks turn green (`success`) from `pending` or `failure`. Default action `wake`. The owner gets "CI is green on #N" and is told to carry on with whatever it was waiting on.
2. **`ready_for_review`:** an open, non-draft PR with CI `success` or `none`, `mergeable == MERGEABLE`, and a review decision that is neither `APPROVED` nor `CHANGES_REQUESTED`. Default action `ask`, so you get an Inbox item saying the PR is green and waiting on review. It fires once per head commit through `fired`, not on a state change. So PRs already in that state when this ships, like #2108, #2109 and #1446, each fire once.
3. **`new_review` per review:** the dedupe key includes the review count (`new_review@<head>#<count>`), so each new review on the same commit fires.
4. **Known PRs aren't dropped:** a PR already in the watcher's state, of any state, stays there while one of its tickets is still watched, even when this poll's search didn't return it. Only open ones are re-read when the search misses them, as now.

## Non-goals

- Stall timers, or waking on silence.
- Changing which agent owns a PR (`owner_of`), or how a stopped owner becomes an Inbox item.
- Any UI change beyond the Inbox items that already render watcher items generically.

## Approaches

- **Chosen: new triggers and fixes inside the existing rules engine.** They go through `watch.rules`, the `fired` dedupe and `act()`, like every other trigger. The user can tune them in config.
- Rejected: a stall timer (above).
- Rejected: folding green-CI into `approved_green`. That one needs an approval, which is exactly what's missing in the stalled cases.

## Design notes

- `config.rs` `Rules` gets `ci_passed` (default `wake`) and `ready_for_review` (default `ask`), and `Rules::action` maps them. `retro.rs` already allows `watch.rules.*`.
- `rules.rs` `on_pr`:
  - add the two triggers
  - the `new_review` key includes the count
  - `headline` adds "CI passed" and "green, waiting on review"
  - `pr_text` gets a body and a to-do for each. `ci_passed`'s to-do: "Continue with what you were waiting on for this PR. If it's done, say so." `ready_for_review` has no to-do; it's news for you.
- `ci_passed` on a PR seen for the first time (`before` is the unseen default with `ci == ""`) doesn't fire. Green checks on a new PR aren't news, and `ready_for_review` covers what needs doing.
- `watch.rs`: when a search misses a known PR whose tickets are still watched, keep it whatever its state. Re-read it (`gh pr view`) only if it's open.

## Tests (Rust, `drovr-core`)

- `ci_passed`:
  - fires on pending→success and failure→success
  - doesn't fire on unseen→success
  - fires once per head
- `ready_for_review`:
  - fires for a green, mergeable, review-required, non-draft PR
  - doesn't fire for a draft, `CHANGES_REQUESTED`, `APPROVED`, a conflict, or failing CI
  - fires once per head
  - fires on a PR whose `before` was already in that state but whose `fired` lacks the key
- `new_review` fires twice for two reviews on the same head.
- A merged PR missing from a poll's search stays in state with its `fired` intact, and no second `pr_linked` is recorded.
- `Rules::default()` has `ci_passed = wake` and `ready_for_review = ask`.

## Docs

- README: the trigger table (around line 362), the `watch.rules` config row (667), and the "Into the record" paragraph if it's affected.
- USAGE: the Watcher Inbox bullet (111) and the `[watch.rules]` step (285).
