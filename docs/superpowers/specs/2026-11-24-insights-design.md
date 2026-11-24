# Insights: design

Status: draft, waiting for approval.

## Why

Retros produce advice, but drovr can't show whether spend and rework fall because of it. Most approved process changes are only noted (7 of 8 in the owner's records), and cheaper models were recommended without counting the rework they caused.

## Goals

- See where spend goes, per week, PR, ticket and retro, by stage, harness and model.
- See rework and time to merge over time, with a marker for each retro and each change put in place.
- Hold every approved recommendation with something drovr enforces where it can, and measure it.
- Judge models on the full cost of a merged PR, including rework and time to merge.

## Non-goals

- Billing accuracy. Costs are what each CLI reports.
- Proving cause. Trends are correlations, and are labeled so.
- Merging PRs or marking them ready.

It has failed if it needs a retro to show anything, if a recommendation can be "approved" with nothing holding it, or if it recommends a cheaper model that slows work down.

## Approaches to storing the data

1. **Tables in the existing SQLite database (picked).** One row per run and per PR round, queried by date. Every process already opens it, and agents without a ticket have a place.
2. **Read the ticket ledgers each time.** Nothing new to store, but it misses agents without a ticket, and stays slow and tied to `workflow.enabled`.
3. **A new JSONL log.** Easy to read by hand, but needs its own indexing for every chart.

## Data

- **`spend`**: when, agent, role, group, tickets, stage, harness, model as started, canonical model, run, cost (null when not reported), tokens, duration, state, retro folder if any. A run that used several models has a row per model. Written at the end of every run, **even with `workflow.enabled` off**. Never prompts or messages. README says so plainly.
- **`pr_outcomes`**: one row per PR: repo, number, tickets, builder, builder's harness and model, first seen, ready, merged or closed, review rounds, pushes after ready, CI failures, threads fixed and declined, lines changed. The builder comes from the `drovr/<agent>` branch, else the ticket's only build agent, else unknown. It is never guessed.
- **`recommendations`**: one per idea, across retros: topic, kind, evidence, how it's held, state, and when it was put in place and measured.
- Existing ledgers are imported once for history. Imported rows are marked, since their model is less certain.

Rework counts against the model that built the PR, whichever model fixed it. Routers (`cloudflare/auto`, `unbiased/pareto`) and unknown versions are left out of model comparisons.

## Screens

A sidebar section, **Insights**. Mockups: https://claude.ai/artifact/5wHixooQDvvVkqpNvQhMjM

- **Overview:** averages, weekly spend against an optional target, cost per merged PR, rework, time to merge, top harnesses and topics. Retro weeks carry a marker, and so do setting changes drovr can date, such as reviewers turned on (the first reviewer that started) or a stage's model changed.
- **Models:** cost with rework, share reworked and median time to merge, compared within build size.
- **Recommendations:** filter by topic and state; accept, change how it's held, or dismiss with a reason.
- **Retros:** moves here from Pipeline, with the Not covered, New work, Covered and Hidden lists.

Shown whether or not the workflow is on; what needs it says so when it's off.

## Recommendations

States: Proposed, In place, Working, No effect, Dismissed. How one is held, strongest first:

| Held by | What it does |
|---|---|
| Gate | drovr checks it at handoff and sends it back if it was skipped |
| Setting | A `config.toml` value, with undo |
| Skill | A new or edited `SKILL.md` |
| Rule | A file drovr adds to every agent's instructions |
| drovr change | A spec, then a builder and a draft PR in the drovr repo |
| Memory | A shared fact; a hint, not a guarantee |
| Reminder | Yours; stays open, and drovr asks again in two weeks |

The default is a gate where drovr can check it, and a rule otherwise. Only "In place" is told to later retros as done. Gates run only commands the user approved, word for word.

A change is measured once 15 PRs have merged after it, against the 15 before.

**Models:** a proposal for a cheaper model needs at least 10 PRs on each side, with rework and time to merge. Approving it starts a 10-build trial. It switches back if median time to merge rises more than 10%, or rework passes the limit set.

**drovr changes:** `retro.source` is the local drovr checkout, and `retro.code_model` the cheap model that reads it, read-only, and writes the spec. A builder starts only when the user says so, in its own worktree. It opens a draft PR and never merges.

## Themes

All colors come from the theme. Every theme in `themes.json` gains `chart-1` to `chart-4`, and tests in `themes.rs` fail if a theme lacks one or two neighbors are too alike, including for color-blind readers. Any color under 3:1 on its panel is allowed only because every chart has labels and a table.

## Related, with its own design

With `stages.review` on, Insights also shows how the reviewer does: what it found, and what others found on the PR after its final review (an escaped finding), by review model and by version of `stages.review_skill`. Repeated escaped findings become proposals to edit that skill, shown as a diff; it is never replaced without approval. "Reviewed the PR's current head" is a gate drovr can check; it is built only if escaped findings show it is needed. PRs built with and without a reviewer are compared, and never mixed when models are compared.

PR feedback triage: batched, read-only triage of review comments, done by the build's reviewer with its skill. Declines go to the Inbox first, then can be allowed per bot; human comments always go to the user.

## Build order

Each is its own PR, and useful alone.

1. `feat:` the spend log, and the import.
2. `feat:` chart colors in every theme, with their tests.
3. `feat:` the Insights overview.
4. `feat:` PR outcomes, and the Models screen.
5. `feat:` retros in Insights, with Covered and Hidden.
6. `feat:` tracked recommendations, with rules, gates and reminders.
7. `feat:` model trials.
8. `feat:` drovr code changes.

## Left to the builder

Table indexes, chart markup (no new chart library), the shortcut key, and the wording of empty states. Coverage stays above 80%, and README and USAGE change with each PR.
