# Standing instructions in every agent's system prompt

## Why

A retro on SRE-3311 and SRE-3462 found that most of the human's "miss" calls broke rules that were already written down: ready commands, CodeRabbit state, commit style, PR body length, agent jargon, `cd`-prefixed commands, AGENTS.md ordering. The rules live in a skill (`drovr-sre-standing-rules`), CLAUDE.md and memory, but they don't reach the agents:

- Leads are never pointed at the skill, and workers started without a brief lose the pointer.
- With `defaults.worktrees = true`, agents work under `~/.local/share/drovr/worktrees/`. A CLAUDE.md or AGENTS.md in the parent folder of the repos never loads.
- drovr writes each agent's system prompt itself, but there's no way to add your own text to it.

## Goal

A config key, `defaults.instructions`: a list of file paths (`~` allowed). drovr adds each file's contents to the system prompt of **every** agent, whatever its role (solo, lead, worker, stage agent, reviewer, retro) and whatever its harness (Claude Code, OpenCode, Codex). The prompt is rebuilt before every run, so an edited file takes effect on each agent's next turn.

## Non-goals

- Per-group or per-ticket instructions.
- A Settings editor for the list. It's edited in the config file like other list keys.
- Letting retro proposals change this key.

## Design

- `config.rs` `Defaults` gets `instructions: Vec<String>`, defaulting to empty.
- In `engine.rs` `system_prompt_file`, after everything else (so the standing rules come last):
  - If the list isn't empty, append `"\n## Standing instructions\n\n{your_name} wants these followed in all your work. They override any general habits.\n"` (through `personalize`, like the other sections).
  - Then, for each path, `\n### <file name>\n\n<contents>\n`.
  - Contents go through a new helper, `prompts::instruction_text(raw: &str) -> String`. It strips a leading YAML frontmatter block (`---` … `---`, as SKILL.md files have) and trims.
  - Each file is capped at 20,000 characters with the existing `truncate`.
  - A path that can't be read adds the line `- drovr couldn't read <path> (<error>). Tell {your_name}.` in place of its contents, so the human finds out through the agent instead of the rules silently going missing.
- Docs:
  - README config table: a `defaults.instructions` row.
  - USAGE: a short "Standing instructions" section saying what it's for, that it applies to every agent and harness, and that edits apply on the next run. Mention the worktree reason a parent CLAUDE.md doesn't load.
  - `config.example.toml`: a commented example under `[defaults]`.

## Approaches

- **Chosen: drovr appends files to the system prompt it already writes.** It works for every harness and role, with no dependence on the CLIs' own instruction loading.
- Rejected: harness-native global files (`~/.claude/CLAUDE.md`, OpenCode's `instructions`). They work today, but they're per CLI, apply outside drovr too, and are invisible from drovr's config. They stay available to the user anyway.
- Rejected: the retro's "a pointer to a skill that must be loaded first". Agents can ignore a pointer, and including the text can't be skipped.

## Tests (Rust, `drovr-core`)

- `instruction_text` strips the frontmatter, leaves text without frontmatter alone, and trims.
- `system_prompt_file`:
  - with two instruction files, the written `system.md` contains both bodies under `## Standing instructions`, after the role text
  - a missing path yields the "couldn't read" line
  - an empty list adds no heading
  - a `~/` path is expanded
- The section appears for a worker, a lead and a solo agent alike.
