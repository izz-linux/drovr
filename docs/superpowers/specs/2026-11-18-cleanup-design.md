# Cleanup: archive old plans, prune resolved Inbox items

## Why

The plans folder (`reports.dir`, `~/.claude/plans`) and the Inbox only grow. The user wants old plans moved out of the way, without deleting them, and resolved Inbox items removed after a while. Both happen on their own every day, with a button and a command to run them now.

## Goals

1. **Archive plans.** Move every top-level entry of the plans folder whose **newest file, searched recursively, is older than `reports.archive_after_days`** into `<plans>/archive/`, keeping its name and everything under it. Top-level entries are loose files and group or ticket folders. `0` means never; that's the default.
   - Never archived: `archive/` itself, `retros/` (a running log), anything starting with `.`, and the folder of a group that still exists in drovr's database.
   - Name collision in `archive/`: append `-2`, `-3` and so on before the extension.
   - Report rows (`report_meta`) for moved files are updated to their new paths.
2. **Prune the Inbox.** Delete items that aren't `pending`, whose `answered_ts` (else `ts`) is older than `inbox.keep_days`. `0` means never; that's the default.
   - Answers an agent can still collect are never deleted: `decision`, `command`, `jira` and `confluence` items, answered or rejected, not yet delivered, for an agent that still exists. Other kinds are never marked delivered, so they can't be guarded that way.
3. **When it runs:**
   - Automatically once a day from the app's background loop, with the last run time kept in `<home>/cleanup.json`.
   - Now: from a **Clean up now** button in Settings, and from `drovr cleanup [--dry-run]`.
   - `--dry-run` lists what would move or be deleted, and changes nothing.
4. **Archived plans are hidden.** The Reports list, the plans folders list and the ticket records (`ledger::list`, which timelines and retros read) skip `archive/`.

## Non-goals

- Restoring from the archive in the UI. Moving a folder back by hand works.
- Deleting archived plans.
- Showing archived reports in Reports.

## Design

- **Config:** `Reports.archive_after_days: u32` (default 0), and a new `[inbox]` section with `keep_days: u32` (default 0).
- **Core:** new `crates/drovr-core/src/cleanup.rs`:
  - `pub struct Outcome { pub archived: Vec<(String, String)>, pub inbox_deleted: usize }` (Serialize): each archived entry is (name, name under `archive/`)
  - `pub fn run(cfg, store, now, dry) -> Result<Outcome>`; with `dry` it only reports what it would do
  - `pub fn due(home, now) -> bool` and `pub fn mark_ran(home, now)`, for the 24-hour gate
  - The age is the max mtime over every file under the entry; for a loose file, its own mtime.
- **Store:** `fn prune_items(before: f64, dry: bool) -> Result<usize>`, with the undelivered-answer guard in the SQL. Plus `fn move_report_meta(from_prefix, to_prefix)`.
- **App:** the 15-second watcher thread also calls `cleanup::run` when `cleanup::due`. A Tauri command `cleanup_now() -> Outcome` backs the Settings button, which toasts "Archived N plans and removed M Inbox items."
- **CLI:** `drovr cleanup` prints what moved and what was removed. `--dry-run` prints the same from a dry run, prefixed with "would".
- **Docs:** README config rows, a USAGE section "Clean up old plans and Inbox items", the CLI reference, and config.example.toml.

## Tests

- **Archiving:**
  - a folder whose newest nested file is old is archived
  - a folder with one fresh nested file isn't
  - loose old files move
  - `archive/`, `retros/` and dot-entries stay
  - a folder for a group in the store stays
  - a name collision gets `-2`
  - `report_meta` paths follow the move
  - `0` days does nothing
  - a dry run reports the same outcome and changes nothing
- **Inbox:** answered, dismissed and rejected items older than the cutoff go; pending items stay; undelivered answers stay; `0` days does nothing.
- **Hidden archive:** `reports::list`, `reports::folders` and `ledger::list` skip `archive/`.
- **Daily gate:** `due` is true with no file, false within 24 hours of `mark_ran`, and true after.
