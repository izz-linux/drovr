# Standing Instructions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `defaults.instructions`, a list of files whose contents drovr adds to every agent's system prompt.

**Architecture:** `Defaults` gets `instructions: Vec<String>`. `Engine::system_prompt_file` appends a "Standing instructions" section built from those files, after every other section. A small pure helper strips a SKILL.md's frontmatter.

**Tech Stack:** Rust (`drovr-core`).

**Spec:** `docs/superpowers/specs/2026-11-18-standing-instructions-design.md`

## Global Constraints

- Config key: `defaults.instructions`, a list of path strings. `~` is expanded with `config::expand_tilde`. Default `[]`.
- The section heading is exactly `## Standing instructions`. Its intro line is `{your_name} wants these followed in all your work. They override any general habits.`, passed through `config::personalize` like the other sections.
- Each file goes under `### <file name>`, where the file name is `Path::file_name`, e.g. `SKILL.md`. If that name is `SKILL.md`, use its parent folder's name instead, e.g. `drovr-sre-standing-rules`, so several skills are distinguishable.
- Each file is capped at 20,000 chars with the existing `truncate`.
- An unreadable file produces the line `- drovr couldn't read <path> (<error>). Tell {your_name}.`
- The section goes **last** in `system.md`, after the review, Datadog and memory sections.
- No new dependencies. Commits are single-line Conventional Commits with no trailers. Run fmt, tests and clippy as separate commands.
- README.md, USAGE.md and config.example.toml are updated in this PR.

## Review Focus

1. **A skill with frontmatter:** the `---` block must not reach the prompt, and a `---` horizontal rule later in the body must survive. (Test.)
2. **An empty list:** no heading, and `system.md` is byte-for-byte what it was before this change for every existing test. (Test, plus the existing suite.)
3. **A missing file:** the agent still starts, and the prompt says which file is missing. (Test.)
4. **Every role gets it:** solo, worker, lead and reviewer. (Test with at least solo, worker and lead.)
5. **A file edited between runs:** the next run's prompt shows the new text, because the prompt is rewritten per run. (Test by calling `system_prompt_file` twice with an edit in between.)

---

### Task 1: `defaults.instructions`

**Files:**
- Modify: `crates/drovr-core/src/config.rs` (`Defaults` struct about :338-360, its `Default` about :633-650)
- Modify: `crates/drovr-core/src/prompts.rs` (add `pub fn instruction_text`)
- Modify: `crates/drovr-core/src/engine.rs:546-609` (`system_prompt_file`)
- Test: `crates/drovr-core/src/prompts.rs` tests (create `mod tests` if none), and `crates/drovr-core/src/engine.rs` tests. `stages.rs:480-570` shows how existing tests read an agent's `system.md` via `engine.agent_dir(name).join("system.md")`.
- Docs: README config table (near the `defaults.*` rows, about :640), a USAGE section "### Give every agent your standing rules" (put it after "### Share one memory across agents", about :182), and `config.example.toml` `[defaults]` (a commented example line)

**Interfaces:**
- Produces: `Defaults.instructions: Vec<String>`, `prompts::instruction_text(raw: &str) -> String`

- [ ] **Step 1: Write the failing tests.**
  - `prompts::tests::instruction_text_strips_frontmatter`:
    - `"---\nname: x\ndescription: y\n---\n\n# Rules\n\na\n\n---\n\nb\n"` gives `"# Rules\n\na\n\n---\n\nb"`
    - `"# Rules\nno front"` is unchanged
    - `"  \n text \n"` gives `"text"`
  - `engine::tests::standing_instructions_reach_every_agent`:
    - temp dir with `a.md` ("Use pnpm.") and `skills/drovr-sre-standing-rules/SKILL.md` (frontmatter plus "No cd prefixes.")
    - `cfg.defaults.instructions` set to both
    - add a group with a lead and a worker, plus a solo agent
    - call `system_prompt_file` for each
    - each `system.md` contains `## Standing instructions`, `### a.md`, `Use pnpm.`, `### drovr-sre-standing-rules` and `No cd prefixes.`, and doesn't contain `description:`
    - the index of `## Standing instructions` is greater than the index of the role text (e.g. `You are the drovr agent`)
  - `engine::tests::missing_instruction_file_is_reported`: the path `/nonexistent/rules.md` gives a prompt containing `drovr couldn't read /nonexistent/rules.md`.
  - `engine::tests::no_instructions_no_section`: an empty list gives no `Standing instructions` in the prompt.
  - `engine::tests::edited_instructions_apply_next_run`: write "v1", build, write "v2", build again, and the second prompt contains "v2" and not "v1".

- [ ] **Step 2: Run them and confirm they fail.**
Run: `cargo test -p drovr-core instruction`
Expected: compile errors (no `instructions` field, no `instruction_text`).

- [ ] **Step 3: Implement.**
  - **Config field:** add it with a doc comment: "Files whose contents go into every agent's system prompt, after drovr's own instructions (`~` allowed). Edits apply on each agent's next run."
  - **`instruction_text`:** if the text, trimmed of leading whitespace, starts with a line that's exactly `---`, drop through the next line that's exactly `---`. Then trim.
  - **`system_prompt_file`:** after the memory block and before writing, build the section following the Global Constraints.

- [ ] **Step 4: Run the tests and confirm they pass.**
Run: `cargo test -p drovr-core instruction`, then `cargo test -p drovr-core` (the full suite, so the existing prompt tests are unchanged).
Expected: PASS.

- [ ] **Step 5: Update the docs.**
  - README row: `| defaults.instructions | [] | Files added to every agent's system prompt (lead, workers, stages, reviewers, solo; every harness), e.g. a standing-rules skill. Edits apply on the next run. |`
  - The USAGE section, in 4-6 sentences:
    - what it's for
    - that it covers agents started without a brief
    - that with worktrees on, a CLAUDE.md or AGENTS.md above the repo never loads, which is why this exists
    - a TOML example
    - that a missing file is reported by the agent
  - config.example.toml: `# instructions = ["~/.claude/skills/my-rules/SKILL.md"]   # added to every agent's system prompt`

- [ ] **Step 6: Run the full checks.**
Run, as separate commands: `cargo fmt --all`, `cargo test -p drovr-core`, `cargo clippy --workspace --all-targets -- -D warnings`. If clippy needs `app/build` and the sidecar, run `cd app && pnpm build` and `cd app && pnpm cli`.

- [ ] **Step 7: Commit.** `git commit -m "feat: add standing instructions to every agent's system prompt"`
