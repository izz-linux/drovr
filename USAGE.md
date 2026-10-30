# Using drovr: commands and workflows

How to get things done with drovr, in the app and from the command line. For what drovr is, setup and configuration, see the [README](README.md).

- [Keyboard shortcuts](#keyboard-shortcuts)
- [Workflows](#workflows)
- [CLI reference](#cli-reference)

## Keyboard shortcuts

| Keys | Does |
| --- | --- |
| ⌘1 … ⌘8 | Agents, Inbox, Reports, Jira, Slack · Confluence, Message bus, Code review, Settings |
| ⌘9 | Pipeline (with `workflow.enabled`) |
| ⌘N | New agent |
| ⌘G | New group |
| Enter | Send the prompt (in an agent's prompt box) |
| Shift+Enter | New line in the prompt box |
| ⌘F | Find in the selected agent's output (Enter: next, Shift+Enter: previous, Esc: close), or filter the Message bus |
| ⌃` | Show or hide the terminal under the agent's output (**View → Toggle Terminal**) |
| Esc | Close a dialog. Once you've typed in it, press Esc again (or click outside again) to discard the typing |
| ⌘S | Save your config (in Settings) |
| Enter | Answer with what you typed (in a decision's answer box) |
| ⌘/ | List the keyboard shortcuts (**View → Keyboard Shortcuts**) |

In the **Inbox**, when you're not typing in a box:

| Keys | Does |
| --- | --- |
| ↓ ↑ or J K | Next / previous item |
| 1 … 9 | Pick that option of a decision, or a Watcher item's **Send to** |
| A | Allow a permission request, or start a worker |
| D | Deny a permission request (with the reason typed in its box), or decline a worker |

Ctrl works in place of ⌘, except in the terminal, where Ctrl keys go to the shell. **View → Theme** switches themes.

## Workflows

### Work a Jira ticket with one agent

1. **Find the ticket:** go to **Jira** (⌘4), type `SRE-3253`, a JQL query, or plain text, and press Enter. Leave the box empty to list your open tickets.
2. **Hand it off:** open the ticket, click **Hand off to agents…**, choose **Single agent**, check the working directory, add any notes, and click **Start**.
3. **Watch it work:** the agent opens in **Agents** (⌘1). It gets the ticket's summary, description, recent comments, and your notes under "Notes from Izz".

```sh
drovr dispatch SRE-3253 solo ~/code/pay-api
```

### Start an agent and have it create its ticket

1. **New agent** (⌘N): give it a name and the task, leave **Jira ticket** empty, and check **Create a Jira ticket for this**. Add notes if you want ("a bug, assigned to me, in the current sprint").
2. The agent writes the ticket first, with your ticket skill (`jira.ticket_skill`), then asks to run its create command: allow it in the **Inbox**.
3. It takes the new key with `set_ticket`, and its work is recorded under that ticket from then on. Then it starts the task.

### Give a ticket to a group you already have

1. Open the ticket and click **Hand off to agents…**, then choose **Existing group**.
2. Pick the group. **Send to** defaults to its lead; pick a member instead if the group has no lead.
3. The lead gets the ticket and is told to plan it with the agents already in the group. If it's busy, it reads the ticket next.

```sh
drovr dispatch SRE-3253 to pay-api                 # to the group's lead
drovr dispatch SRE-3253 to pay-api pay-api-sweeper # to one member
```

### Start a new group with an orchestrator

From a ticket: **Hand off to agents… → New group + orchestrator**. Or use **New group** (⌘G), check **Start an orchestrator**, and give it a goal. The `<group>-lead` agent plans the work, spawns workers, and brings you decisions.

```sh
drovr group checkout-v2 ~/code/web "Move checkout to the new payments API"
drovr dispatch SRE-3253 group ~/code/pay-api
```

### Build a group from plans you already wrote

1. Put your plans and prompts in a folder under `~/.claude/plans/`, e.g. `~/.claude/plans/Pay API/coordinator.md`.
2. **New group** (⌘G): choose that folder under **Plans folder**, leave **Start an orchestrator** off, and click **Create, then add agents**.
3. **New agent** opens with the group selected. Pick a file under **Prompt from file**: it fills in the task, which you can edit. Then **Start**.
4. Add the group's other agents with **+ agent** on the group's header in **Agents**. Add a lead any time with **+ lead**.

### Pick up an existing Claude Code session

1. **New agent** (⌘N) → **Existing Claude session**.
2. Choose a recent session, or paste the ID you'd give `claude --resume <id>`. The working directory fills in from the session.
3. Leave Task empty to carry on, or type what to do next. Turn on **Fork** if the session is still open somewhere else.

```sh
drovr sessions                                      # recent sessions: id, folder, first prompt
drovr adopt 225e9ac0-9c9c-4f7c-927e-49054bff82a0 env-coord
drovr adopt 225e9ac0-9c9c-4f7c-927e-49054bff82a0 env-copy --fork "Now write the tests"
```

### Answer what agents need from you

Everything waiting on you is in the **Inbox** (⌘2). An agent shows **blocked** while something of its is there.

- **Decision:** click an option or type an answer.
- **Command:** edit if needed, then choose one:
  - **Run & return output** runs it in the agent's directory and sends the output back to the agent. It runs in drovr, not the Inbox, so you can go elsewhere while it does; the agent is woken with the output when it finishes. It's stopped after `commands.timeout_seconds` (10 minutes by default), and the agent gets what it printed by then, marked "(timed out)". Raise the limit for long commands such as several `terraform plan`s.
  - **Run in terminal** types it into the agent's terminal in drovr. Check it, press Enter, then click **Mark as run**: the agent gets what it printed.
  - **Run in herdr pane** runs it in herdr.
  - **Copy** it to run anywhere, then **Mark as run**. Paste the output or a note in the box first, and the agent gets it.
  - **Reject** it.
- **Permission:** **Allow**, or **Deny** with a reason.
- **Spawn** (with `orchestrator.auto_spawn = false`): a lead wants to start a worker. **Start worker** or **Decline**.
- **Finished:** read the summary, then **Review changes**.
- **Watcher** (with `workflow.enabled`): something changed on a ticket or its PR (checks failed, a review, a conflict, ready to merge, merged or closed, or a Jira change) and its rule in `watch.rules` is `"ask"`. **Send to <agent>** passes it on with the details, or dismiss it.
- **Confluence:** an agent wrote a page. Read the diff against the live draft and the full page, then **Save as draft in Confluence** or **Send back** with notes. drovr reads the page again before saving and shows you a new diff if it changed; it never publishes.
- **Handoff** (with `workflow.enabled`, in a group created while it was on): a stage finished. Read its handoff, then **Approve** (the lead starts the next stage; change its model in the box if you want) or **Send back** with notes.
- **Jira draft:** a comment or status move drafted by drovr (when a PR merges) or by an agent. Edit it, then **Post to Jira** (drovr runs `jira.comment_cmd` / `jira.transition_cmd`), or **Discard**.
- **Once only:** while an answer is being sent or a command runs, that item's buttons are disabled. A command runs once, and an item that's already resolved can't be answered or run again.

```sh
drovr inbox
drovr answer 7 "Full refunds only"
drovr answer 8 allow
drovr reject 9 "Not on prod"
drovr run 10 local
drovr ran 11 "Pushed; PR #482 opened"
drovr answer 12 "Send to sre-3229-build"          # a Watcher item
drovr answer 13 "Approve model=claude-opus-5-5"   # a Handoff (or just Approve)
drovr answer 14 "Send back: cover the rollback"   # a Handoff, back with notes
drovr answer 15 "Merged in o/r#7"                 # a Jira draft: posts this text
drovr answer 16 approve                           # a Confluence page: saves the draft
```

### Use the terminal

Press ⌃` (or **Terminal** in the agent's header) to open a terminal under the agent's output; press it again to hide it.

- **Groups:** the group's agents share one terminal, which starts in your home folder like a new iTerm2 window. Use it to orchestrate: check on repos, run `gh`, `drovr` commands, anything.
- **Solo agents:** the terminal starts in the repo or worktree the agent last worked in, ready for a quick `git status` or test run.
- **Your setup:** your aliases, functions and prompt work, because it runs your shell as a login shell.
- **Copying:** select text to copy it (`terminal.copy_on_select`), or use ⌘C.
- **Hiding it:** hiding it doesn't stop anything; the shell and its scrollback stay until you quit drovr.
- **Stuck command:** if ^C doesn't stop it (a `terraform` shutting down, an `op run` waiting for 1Password), click **Stop job** in the terminal's bar. **Restart shell** starts over with a fresh shell.

### Talk to an agent

- **Prompt one agent:** select it in **Agents** and type in the prompt box. Enter sends. If it's busy, the text shows above the box as **Queued** until the agent reads it. **Send now** on a queued message stops the run and sends it straight away. **✕** takes it back into the box to edit.
- **Copy from the output:** select text in an agent's output to copy it (`defaults.copy_on_select`).
- **Message a group:** go to **Message bus** (⌘6), pick the group, and send to everyone or to one agent. Idle agents wake up to read it. Type in **Filter messages** (⌘F) to show only messages that mention an agent, a ticket or a word. Messages to and from you show your name (`defaults.your_name`).
- **Pass along context:** in **Slack · Confluence**, choose an agent under **Send results to** before or during a search. When the search finishes, its results go to that agent, even if you've switched views. **send to agent** on a single result sends just that one.

```sh
drovr prompt pay-api-lead "Hold off on the harness until Bun 1.4.2 ships"
drovr msgs pay-api
```

### Review what agents produced

- **Reports** (⌘3): HTML reports in `~/.claude/plans/`, grouped by folder, newest first, with new ones marked; if any are unread, the newest opens straight away. A revised report replaces the agent's earlier copy, and older copies with the same title fold under the newest as earlier versions. Open one, then send feedback to an agent from the bottom bar.
- **Code review** (⌘7): pick an agent to see each repo and worktree it worked in, with what changed on its branch (committed or not) and the diffs. It works when the agent started in a folder above your repos. Send feedback about a file. If your plans folder is in a git repo (say `~/.claude`), only the part this agent worked in is shown there.
- **Agents:** each agent's full output, tool calls and cost. **Open in herdr** opens its session interactively, or **Copy attach cmd** for any terminal.
- **Timeline:** with `workflow.enabled`, everything that happened on a ticket (see [Read a ticket's timeline](#read-a-tickets-timeline)).

```sh
drovr log pay-api-lead 100
drovr attach pay-api-lead          # prints: cd … && claude --resume …
```

### Look things up in Jira, Confluence and Slack

**Jira** (⌘4) and **Slack · Confluence** (⌘5) search through your MCP servers using Haiku. While a lookup runs, you see what it's doing and how long it's taken, and **Cancel** stops it. **Ask** answers a free-form question with every MCP you have.

```sh
drovr ask "What did #sre decide about the payments API last week?"
```

### Stop, resume and clean up

- **Stop:** **Stop** ends an agent's current run, and **Resume** continues the same conversation.
- **After a reboot or crash:** agents show **interrupted**. Click **Resume all**, or set `defaults.auto_resume = true` to resume them automatically.
- **Finished with an agent:** **✕** next to its name stops it and removes it, with its output. Click it, then click **Remove?** within a few seconds to confirm. Anything of its still waiting in the Inbox is dismissed. Its Claude Code session stays on disk, so `claude --resume` can still open it.
- **Its worktree:** with `defaults.worktrees`, a clean worktree is removed with the agent, and one with uncommitted changes is kept, which drovr tells you. The `drovr/<name>` branch always stays, so its commits are safe, and starting an agent with the same name again picks the branch (or the kept worktree) back up.
- **Groups:** **stop all** on a group's header stops every agent in it, and **✕**, then **Remove?**, removes the group and its agents.
- **Quitting drovr** doesn't stop headless agents, and herdr agents are never affected.

```sh
drovr ls
drovr stop pay-api-sweeper
drovr resume pay-api-sweeper "Continue with the cleanup"
drovr rm pay-api-sweeper
```

### Run an agent with Codex (experimental)

1. Install the Codex CLI and log in (`codex login`).
2. In **New agent**, **New group** (with an orchestrator) or **Hand off**, set **Run with** to **Codex**. It's greyed out until drovr finds `codex` (`codex.command`).
3. The agent works like any headless agent: messages, the Inbox, reports and Code review. It runs in Codex's sandbox (`codex.sandbox`), so it proposes commands that need more as Inbox commands instead of asking for permission.

```sh
drovr new --harness codex fix-ci ~/code/api "Make CI green"
drovr group --harness codex checkout-v2 ~/code/web "Move checkout to the new payments API"
```

Set `defaults.harness = "codex"` to make Codex the preselected choice. Codex agents run headless only, and can't pick up existing Claude Code sessions.

### Turn the ticket workflow on or off

Everything in the next few sections is behind one switch. In **Settings** (⌘8), set it and save (⌘S); it applies straight away, and agents pick it up on their next run:

```toml
[workflow]
enabled = true    # false: drovr works as it did without it
```

Turning it on doesn't change groups you already have: only groups created while it's on work in stages. They still get the record, the timeline and the watcher.

### Follow your sprint and its PRs

1. **Set up the watcher** in Settings: your Jira command (twg), the tickets to follow and where their PRs live.

   ```toml
   [jira]
   provider = "command"
   get_cmd = "twg jira workitem get {key} --comments -o json"
   search_cmd = "twg jira workitem query --jql {query} -o json"

   [watch]
   jql = "project = SRE AND sprint in openSprints() AND assignee = currentUser()"
   github_repos = ["yourorg/*"]
   ```
2. **Pipeline** (⌘9): each ticket's Jira status, its agents, its PRs (CI ✓ ✗ ●, review state, conflicts, drafts), what's waiting for you and what it's cost. A PR belongs to a ticket when its title has the key. **Refresh now** reads Jira and GitHub straight away; errors show at the top.
3. **Decide what happens on a change** with `[watch.rules]`: per trigger (`ci_failed`, `changes_requested`, `new_review`, `conflict`, `approved_green`, `merged`, `pr_closed`, `jira_changed`), `"wake"` the agent with the details, `"ask"` you in the Inbox, or `"ignore"`. Start with `"ask"` everywhere and loosen one at a time.
4. **Jira updates:** with `jira.comment_cmd` set, a merge drafts a comment for you to edit and post (and a move, with `watch.merged_transition`). With `watch.dispatch_transition`, dispatching a ticket moves it straight away, say to In Progress. For twg, a transition is its id: `twg jira workitem transition --id SRE-123 -o json` lists them without moving anything.

### Run a ticket through stages

1. **Dispatch it as a group:** **Dispatch…** in Pipeline, or **Hand off to agents…** in Jira, defaults to **New group + orchestrator** with the workflow on. The `<ticket>-lead` starts the first stage (by default triage → research → blueprint → build → close).
2. **Approve each handoff** in the Inbox: read it, change the next stage's model in the box if you want, and **Approve**, or **Send back** with notes. The finished stage's agent stops; the lead starts the next.
3. **Long builds** write a checkpoint and continue in a fresh session once a session reaches `stages.build_checkpoint_tokens` or `stages.build_checkpoint_usd`.
4. **Models per stage:** `[stages.models]`, e.g. `triage = "claude-haiku-5-5"`.

### Read a ticket's timeline

**Timeline** in an agent's header, **timeline** on a group, or **Timeline** in Pipeline: everything that happened on the ticket, day by day, with totals at the top (runs and cost, questions asked and declined, decisions, wakes). Filter by runs, decisions and the Inbox, messages, status and reports, agents and wakes, Jira and PRs, stages, or retro, and click an event to see all of it. It's read from `ledger.jsonl` in the ticket's folder; **Show folder** opens it.

### Run a retro

1. **Start it:** **Start retro** in a ticket's timeline, or **Retro…** in Pipeline for several tickets, with a note if there's something to look at in particular.
2. A retro agent reads the records and the numbers drovr works out (cost by stage and model, rework, dismissals, misses) and proposes changes to stage models, checkpoint limits, wake rules, the process and your skills, each with its evidence.
3. **Decide:** under **Retros** in Pipeline, approve or dismiss each, or **Approve all**. Approving applies it: a config change keeps your file's comments and saves the previous one as `config.toml.before-retro`; a skill is written to `retro.skills_dir`.

### Have an agent write a Confluence page

1. **Set it up once** in Settings (the twg templates are in the README under [Pages agents write](README.md#pages-agents-write)): `confluence.page_get_cmd`, `page_update_cmd` and `page_create_cmd`. Agents get `publish_confluence` from their next run.
2. **Tell the agent where:** the page to update (a blank page you made works), or a page or folder to create it under.
3. **Review it in the Inbox:** the diff against the live draft and the full page. **Save as draft in Confluence**, or **Send back** with notes. drovr reads the page again before saving; nothing is ever published, so you publish it in Confluence.

### Choose models

Claude Code agents use `defaults.model`, or your Claude Code default when it's empty; Codex agents use `codex.model`, or Codex's default. Lookups use `agent_queries.model`, which is `haiku` by default. With the ticket workflow, each stage can have its own (`stages.models.<stage>`, or the box on a handoff), and so can the retro (`retro.model`). To use Opus for the work and Haiku for searching, set this in **Settings**:

```toml
[defaults]
model = "claude-opus-5-5"

[agent_queries]
model = "haiku"
```

## CLI reference

The `drovr` CLI does what the app does, from a shell, a herdr pane, or a script. It uses the same database, so the app shows whatever the CLI does. Put it on your PATH with `cargo install --path crates/drovr-cli`.

Agent and group names are lowercase letters, digits, `-` and `_`, starting with a letter. `new`, `adopt` and `group` lowercase the name and turn spaces into dashes, so `drovr new "Fix login" …` starts `fix-login`; the name it prints is the one to use afterwards.

| Command | What it does |
| --- | --- |
| `drovr ls` | List agents: group, role, state, cost, status |
| `drovr new <name> <cwd> <task…>` | Start a solo agent |
| `drovr group <name> <cwd> <goal…>` | Start a group with an orchestrator |
| `… --harness claude\|codex` | With `new`, `group` or `dispatch`: the CLI that runs the agents (default `defaults.harness`) |
| `drovr dispatch <TICKET> [solo\|group] [cwd]` | Hand a Jira ticket to a new agent or a new group |
| `drovr dispatch <TICKET> to <group> [agent]` | Hand a Jira ticket to an existing group: its lead, or the agent you name |
| `drovr sessions` | List recent Claude Code sessions |
| `drovr adopt <session-id> <name> [--fork] [task…]` | Start an agent from an existing Claude Code session |
| `drovr prompt <agent> <text…>` | Send an agent a prompt (queued if it's busy) |
| `drovr stop <agent>` | Stop an agent's current run |
| `drovr resume <agent> [text…]` | Resume a stopped, failed or interrupted agent |
| `drovr rm <agent>` | Stop an agent and forget it; says if its worktree was kept |
| `drovr log <agent> [lines]` | Show an agent's output |
| `drovr attach <agent>` | Print a command that opens the agent's session in a terminal |
| `drovr inbox` | List everything waiting for you: decisions, commands, permission requests, worker requests, and with the ticket workflow, Watcher items, Jira drafts, handoffs and Confluence pages |
| `drovr answer <id> <answer…>` | Answer an item: a decision's answer; `allow` / `deny` a permission request; `Send to <agent>` a Watcher item; `Approve [model=<model>]` or `Send back: <notes>` a handoff; the text to post for a Jira draft; `approve` a Confluence page |
| `drovr reject <id> [reason…]` | Reject an inbox item |
| `drovr ran <id> [output…]` | Mark a proposed command as run by you; the agent gets the output or note |
| `drovr run <id> [local\|herdr]` | Run a proposed command; with `local`, the output goes back to the agent |
| `drovr msgs [group]` | Show the message bus |
| `drovr ask <question…>` | Ask a one-shot agent that uses your MCP servers |
| `drovr config` | Print where drovr reads its config and keeps its data, as JSON |
| `drovr help` | Show this list |

Agents and run wrappers call two internal commands: `drovr mcp --agent <name>` is an agent's MCP server, and `drovr hook finished <agent>` runs when a headless run exits.
