# Using drovr: commands and workflows

How to get things done with drovr, in the app and from the command line. For what drovr is, setup and configuration, see the [README](README.md).

- [Keyboard shortcuts](#keyboard-shortcuts)
- [Workflows](#workflows)
- [CLI reference](#cli-reference)

## Keyboard shortcuts

| Keys | Does |
| --- | --- |
| ⌘1 … ⌘7 | Agents, Inbox, Reports, Jira, Slack · Confluence, Message bus, Code review |
| ⌘8 | PRs (once `[review]` has a Slack channel or a GitHub team, or PRs are listed) |
| ⌘9 | Pipeline (with `workflow.enabled`) |
| ⌘, | Settings (**drovr → Settings…**) |
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
| A | Allow a permission request (not while its push preview is open), or start a worker |
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

### Pick up an existing Claude Code or OpenCode session

1. **New agent** (⌘N) → **Existing session**.
2. Choose a recent session, or paste the ID you'd give `claude --resume <id>`. The working directory fills in from the session. For an OpenCode session, set **Run with** to **OpenCode** to list OpenCode's sessions instead, or paste its `ses_…` ID, which picks OpenCode for you.
3. Leave Task empty to carry on, or type what to do next. Turn on **Fork** if the session is still open somewhere else.

```sh
drovr sessions                                      # recent sessions: id, folder, first prompt
drovr adopt 225e9ac0-9c9c-4f7c-927e-49054bff82a0 env-coord
drovr adopt 225e9ac0-9c9c-4f7c-927e-49054bff82a0 env-copy --fork "Now write the tests"
drovr sessions --harness opencode                   # OpenCode's sessions
drovr adopt ses_4f2a1c login-fix                    # a ses_… ID is an OpenCode session
```

### Answer what agents need from you

Everything waiting on you is in the **Inbox** (⌘2), oldest first: new items join the end. An agent shows **blocked** while something of its is there.

- **Decision:** click an option or type an answer.
- **Command:** edit if needed, then choose one:
  - **Run & return output** runs it in the agent's directory and sends the output back to the agent. It runs in drovr, not the Inbox, so you can go elsewhere while it does; the agent is woken with the output when it finishes. It's stopped after `commands.timeout_seconds` (10 minutes by default), and the agent gets what it printed by then, marked "(timed out)". Raise the limit for long commands such as several `terraform plan`s.
  - **Run in terminal** types it into the agent's terminal in drovr. Check it, press Enter, then click **Mark as run**: the agent gets what it printed.
  - **Run in herdr pane** runs it in herdr.
  - **Copy** it to run anywhere, then **Mark as run**. Paste the output or a note in the box first, and the agent gets it.
  - **Reject** it.
  - **View push** (when it runs `git push`, `gh pr create` or `gh pr edit`) opens what the push adds inside the card: **This push** (the commits GitHub doesn't have yet, and their diff), **Whole branch** (the branch against its base, with this push's added lines highlighted, and earlier additions and all removed lines dimmed; hidden on a new branch) and **PR summary** (the `--body-file` or `--body` text). It reads the text currently in the box, so an edited command previews as edited. **Refresh** recomputes an open preview, and the card says so if the command has changed since. If drovr can't find a push it can preview, the card says that, which doesn't mean the command won't push. Nothing is run or fetched.
- **Permission:** **Allow**, or **Deny** with a reason. A Bash command that pushes has the same **View push**. Unless the command gives a full path, drovr starts from the agent's working folder and says so; check the repo and branch on the summary line.
- **Spawn** (with `orchestrator.auto_spawn = false`): a lead wants to start a worker. **Start worker** or **Decline**. The box next to it is the model the worker runs on: the lead's pick if it made one, else empty for the default shown in it (a stage's `stages.models`, or `defaults.model`). Type another to override it.
- **Finished:** read the summary, then **Review changes**.
- **Watcher** (with `workflow.enabled`): something changed on a ticket or its PR (checks failed or passed, a review, a conflict, waiting on review, ready to merge, merged or closed, or a Jira change) and its rule in `watch.rules` is `"ask"`. **Send to <agent>** passes it on with the details, or dismiss it.
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

- **Reports** (⌘3): HTML reports in `~/.claude/plans/`, grouped by folder, newest first, with new ones marked; if any are unread, the newest opens straight away. Folders collapse and expand with a click, and stay that way; a collapsed folder opens on its own when Reports takes you to an unread report in it. A revised report replaces the earlier copy, whichever agent published it, and older copies of the same title are deleted; an agent keeps earlier versions only when you ask it to, and those fold under the newest. Open one, then send feedback to an agent from the bottom bar.
- **Code review** (⌘7): pick an agent to see each repo and worktree it worked in, with what changed on its branch (committed or not) and the diffs. It works when the agent started in a folder above your repos. Send feedback about a file. If your plans folder is in a git repo (say `~/.claude`), only the part this agent worked in is shown there.
- **Agents:** each agent's full output, tool calls and cost. A herdr agent's session shows the same way; **Pane** switches to its pane's screen, and when it's waiting for you there, its last line has a button to the pane. **Open in herdr** opens its session interactively, or focuses its pane if it's already running in herdr. Use **Copy attach cmd** for any terminal.
- **Timeline:** with `workflow.enabled`, everything that happened on a ticket (see [Read a ticket's timeline](#read-a-tickets-timeline)).

```sh
drovr log pay-api-lead 100
drovr log pay-api-lead 100 --pane  # a herdr agent's pane as it is on screen
drovr attach pay-api-lead          # prints: cd … && claude --resume …
                                   # (an OpenCode agent: cd … && OPENCODE_CONFIG=… opencode --standalone --session …)
```

### Review PRs posted in Slack or requested from your team

1. **Set what to watch** in `[review]`:

   ```toml
   [review]
   slack_channels = ["#sre-reviews"]
   github_teams = ["my-org/sre"]
   repos_dir = "~/code"
   ```

   PR reviewers follow `stages.review_skill` and run on `stages.models.review` (harness `defaults.harness`). To only review PRs in some GitHub orgs, set `owners = ["my-org"]`.

   Slack is read through your Slack provider, and `gh` must be signed in. With `provider = "agent"`, each poll is a short `claude -p` run, so it costs a little each time; raise `poll_secs` (600 by default) to poll less. `drovr review poll` looks right away.
2. **Open PRs (⌘8).** Each PR waiting on you is listed once, in **New** until you decide and in **Reviewing** once it has a reviewer. A row shows `owner/repo#n`, the title, the author, where it came from (`#channel`, or `team org/slug`), its age, CI (✓ ✗ ●), GitHub's review decision and your own latest review (waiting on you, you approved, you requested changes, you commented). Closed PRs and your own aren't offered. A draft requested from your team is offered once it's ready; one linked in Slack is skipped. Slack is read back one day on the first poll, and never more than 7. PRs no longer go to the Inbox: new ones raise a notification while drovr is in the background ("PR to review", or "N PRs to review"), the PRs item in the sidebar shows how many are new, and the Dock badge is the Inbox count plus that number. Each poll (`review.poll_secs`) refreshes every listed PR's state, CI and reviews; **Refresh** polls right away.
   - **New:** **Dispatch reviewer** starts the reviewer, **Dismiss** hides the PR, **Open on GitHub** opens it. If the reviewer can't start (a clone or fetch failed), a message says why and the PR stays in New.
   - **Reviewing:** it shows "Starting…" while the reviewer starts, then the reviewer's state (working, idle, reported, error). **Open reviewer** goes to the agent in Agents, where you can tell it to post. **Report** opens its "Review owner/repo#n" report once there is one. **Dismiss** removes the reviewer, its worktree and branch, and hides the PR.
   - **Lifecycle:** a PR leaves the list when you dismiss it, or when GitHub shows it merged or closed (the reviewer is removed too, per `review.remove_when_closed`). A PR you dismissed comes back if someone posts it in a watched channel after you dismissed it. With `review.auto_dispatch = true` a PR starts in Reviewing, and in New only if the reviewer couldn't start. `drovr review <url>` lists the PR in Reviewing too.
3. **Let it work.** The reviewer gets its own folder, and the PR's head is checked out in `./pr` inside it, from the clone in `<repos_dir>/<repo>` (cloned with `gh` if it's missing). It runs outside `pr`, so the PR's own agent config (hooks, MCP servers, OpenCode and Codex config) is never loaded. It reads the PR, doesn't run its code, scripts or tests unless you ask, publishes a report titled "Review owner/repo#n" and messages you a summary. It stays running, so you can ask follow-ups.
4. **Tell it to post.** It changes nothing on GitHub until you say so, for example "post this as a review with those two comments". That's a rule in its instructions, so for a hard stop keep `gh pr review` and `gh pr comment` out of `permissions.auto_allow`: each post then asks in the Inbox.
5. **Clean up.** **✕**, then **Remove?**, deletes its worktree and branch even if it has uncommitted files; its PR stays in **PRs** as *reviewer gone* until you dismiss it or it merges. With `review.remove_when_closed = true` (the default), a reviewer that isn't working is removed once its PR merges or closes.

To review one PR by hand, run `drovr review <pr-url>`. The phone page doesn't show PRs yet.

### Look things up in Jira, Confluence and Slack

**Jira** (⌘4) and **Slack · Confluence** (⌘5) search through your MCP servers using Haiku. While a lookup runs, you see what it's doing and how long it's taken, and **Cancel** stops it. **Ask** answers a free-form question with every MCP you have.

```sh
drovr ask "What did #sre decide about the payments API last week?"
```

### Query Datadog with pup

1. Install pup and log in once: `brew tap datadog-labs/pack && brew install datadog-labs/pack/pup`, then `pup auth login`.
2. In Settings, set `datadog.pup = true`, and `datadog.site` if your Datadog isn't US1.
3. Ask an agent to investigate ("correlate the checkout errors with latency since 14:00"). Its read-only `pup` queries (logs, metrics, dashboards, APM) run without asking. A write, `pup auth`, or a pup command piped into something else comes to the **Inbox** as a permission.

### Share one memory across agents

1. In Settings, set `memory.dir` to the memory folder to share, e.g. `~/.claude/projects/<project>/memory` for one Claude Code already keeps (`ls ~/.claude/projects`).
2. Agents pick it up on their next run, wherever they work. Leads and solo agents save to it. Workers send what they'd save to their lead ("For memory: …"), and the lead decides.
3. Keep `MEMORY.md` under 200 lines; past that, Claude Code stops loading it.

### Give every agent your standing rules

Set `defaults.instructions` to a list of files, and drovr adds their contents to every agent's system prompt, after its own instructions: leads, workers, stage agents, reviewers and solo agents, on every harness. It covers agents started without a brief too, which is where a pointer to a skill gets lost. With `defaults.worktrees` on, agents work under drovr's worktrees folder, so a `CLAUDE.md` or `AGENTS.md` in a folder above your repos never loads, which is why this exists. A skill's frontmatter is left out. drovr reads the files before each run, so an edit applies on the agent's next run. A file drovr can't read is reported by the agent, which tells you, so a typo doesn't silently drop your rules.

```toml
[defaults]
instructions = ["~/.claude/skills/my-rules/SKILL.md", "~/rules/commits.md"]
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

### Clean up old plans and Inbox items

Both are off until you set them (`0` means off):

- `reports.archive_after_days`: a plan or folder in `reports.dir` whose newest file is older than this many days moves, whole, into `<reports.dir>/archive/`. Nothing is deleted, and a name already in `archive/` gets `-2`, `-3` and so on. Archived plans no longer show in Reports or the ticket timelines. A group's folder, `retros`, `archive`, symlinks and anything starting with `.` are never moved.
- `inbox.keep_days`: resolved Inbox items older than this many days are removed. Pending items, and an answer its agent hasn't collected yet, stay.

It runs once a day while drovr is open. To run it now, click **Clean up now** in Settings, or use the CLI. `--dry-run` lists what would happen and changes nothing:

```sh
drovr cleanup --dry-run
drovr cleanup
```

A setting you change applies at the next daily run; click **Clean up now** to apply it straight away.

Problems with single entries are printed as `error: …` and don't stop the rest; the command then exits 1. `reports.dir` must be an absolute folder that isn't `/` or your home folder or above it.

### Check in from your phone

With `[remote] enabled = true` (README → [Phone access](README.md#phone-access)), drovr serves a page for your phone with the Inbox, agents and their output, code review, reports and, with the ticket workflow, the Pipeline.

1. Restart drovr, then scan the QR code in Settings → **Phone** (or from `drovr remote`). It signs the phone in.
2. **Inbox** has the same actions as the app's. Running a proposed command asks you to confirm first.
3. **Agents:** tap one to see its output, prompt it, stop or resume it, or open **Code review** for its diffs.
4. For notifications, subscribe to a hard-to-guess topic in the ntfy app, set `remote.ntfy_topic` to it, and run `drovr remote --test`. Tapping a notification opens that item.
5. With the ticket workflow, **Pipeline** shows your sprint. Tap **Dispatch…** on a ticket nobody's working on to hand it to a single agent, a new group with a lead, or an existing group.
6. For HTTPS with your own certificate, set `remote.tls_cert` (`fullchain.pem`), `remote.tls_key` (`privkey.pem`) and `remote.public_url` (`https://drovr.example.com:7979`, a name the certificate covers that points at the Mac), then restart drovr. Renewed certificates are picked up without a restart.

```sh
drovr remote          # the sign-in link and QR code
drovr remote --test   # send a test notification
drovr serve           # the page and notifications without the app
```

### Run an agent with OpenCode

1. Install OpenCode 2 and connect a provider (`opencode auth login`). Set `opencode.model` to a `provider/model` from `opencode models` if you don't want OpenCode's default.
2. In **New agent**, **New group** (with an orchestrator) or **Hand off**, set **Run with** to **OpenCode**. It's greyed out until drovr finds `opencode` (`opencode.command`).
3. The agent works like any headless Claude Code agent: messages, the Inbox, reports, Code review, costs, and permission requests. Its tool calls are checked the same way (`permissions.auto_allow`, `defaults.permission_mode`), so `shell` commands come to your Inbox as **Bash** permission requests.

```sh
drovr new --harness opencode fix-ci ~/code/api "Make CI green"
drovr group --harness opencode checkout-v2 ~/code/web "Move checkout to the new payments API"
```

Set `defaults.harness = "opencode"` to make OpenCode the preselected choice. **Run in → herdr pane** runs it in OpenCode's TUI in a herdr tab, where you answer its permission prompts yourself; install herdr's OpenCode integration (`herdr integration install opencode`) so herdr reports its state and session exactly. OpenCode's TUI can't fork a session, so adopt with **Fork** runs headless. **Copy attach cmd** opens a headless one in OpenCode's TUI.

drovr starts every OpenCode run and TUI with `--standalone`: a server of its own rather than OpenCode's background service, which wouldn't read the config drovr writes for the agent.

#### OpenCode through a gateway or custom provider

drovr leaves OpenCode's providers to your own config. The config drovr writes for an agent adds only its MCP server, system prompt and permission plugin (and, for the TUI, `opencode.model` and `opencode.agent`), and OpenCode reads it on top of `~/.config/opencode/opencode.json`. Your providers, `enabled_providers`, MCP servers and the plugins in `~/.config/opencode/plugins/` (a token refresher, say) still apply to drovr's agents. Once `opencode run` works in a terminal:

1. Set `opencode.model` to the gateway's model exactly as `opencode models` lists it, for example `"my-gateway/anthropic/claude-sonnet-5.5"`. A gateway's IDs can differ from the provider's own (dots rather than dashes), and one it doesn't know fails the run with `provider.no-route`. Leave it empty to use the `model` in your OpenCode config.
2. If **Run with → OpenCode** says `opencode not found`, set `opencode.command` to its full path (for example `/Users/you/.opencode/bin/opencode`).
3. Allow the gateway's MCP tools by Claude Code's names: a server `my_gateway`'s tool `search` is `mcp__my_gateway__search` in `permissions.auto_allow`.

#### `command not found` for `gh`, `opencode` or `claude`

An app opened from the Dock starts with macOS's bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so drovr asks your shell for yours at startup: first your interactive login shell (`$SHELL -l -i`, which reads `~/.zshrc`; up to 20 seconds, plus up to 3 more to stop it), then a non-interactive login shell (`$SHELL -l`, which reads `~/.zprofile`; up to 10 seconds, plus up to 3). The PATH that worked is saved to `login-path` in drovr's state folder, and drovr uses that saved one if neither shell answers (a slow first shell after a reboot, say). It always appends `/opt/homebrew/bin`, `/opt/homebrew/sbin`, `/usr/local/bin`, `~/.local/bin`, `~/.opencode/bin` and `~/.cargo/bin` when they exist and aren't already there. Both probes run with no stdin and no terminal (and `SSH_ASKPASS_REQUIRE=never`, `GIT_TERMINAL_PROMPT=0`, `DISABLE_AUTO_UPDATE=true`), so a prompt fails at once instead of blocking. With a saved PATH drovr starts at once on it and settles in the background; on the very first launch (nothing saved) its background work waits, at most about 41 seconds in the worst case, before it first runs `gh`, `claude` or `opencode`.

**Settings** shows which one drovr used ("PATH: from your shell", "from your login profile", "saved from an earlier start", or "macOS's default"), and drovr logs `drovr: PATH from <source> (<ms> ms)` to stderr. If your interactive shell never answers, something in your `~/.zshrc` is probably waiting for input (an `ssh-add` passphrase, an oh-my-zsh update question, an MFA helper); run `zsh -l -i -c 'echo done'` in a terminal to see it, and if that is slow rather than waiting for a prompt, look for a stale lock file left by a tool such as pyenv (`~/.pyenv/shims/.pyenv-shim`; delete it if no `pyenv rehash` is running). When a probe or a command times out, drovr sends it SIGTERM and gives it up to 3 seconds to clean up before SIGKILL, so it doesn't leave such locks behind. If it says macOS's default, or your shell is slow, either move your PATH setup (Homebrew's `eval "$(/opt/homebrew/bin/brew shellenv)"`, say) into `~/.zprofile`, which the fallback reads, or set full paths: `watch.gh`, `opencode.command`, `claude.command`. Set `DROVR_KEEP_PATH` to skip all of this.

Costs come from what OpenCode reports for each step. If your OpenCode setup has no prices for a gateway's models, its runs show $0 and `defaults.cost_warning_usd` never flags them.

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

Everything in the next few sections is behind one switch. In **Settings** (⌘,), set it and save (⌘S); it applies straight away, and agents pick it up on their next run:

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
3. **Decide what happens on a change** with `[watch.rules]`: per trigger (`ci_failed`, `ci_passed`, `changes_requested`, `new_review`, `ready_for_review`, `conflict`, `approved_green`, `merged`, `pr_closed`, `jira_changed`), `"wake"` the agent with the details, `"ask"` you in the Inbox, or `"ignore"`. Start with `"ask"` everywhere and loosen one at a time.
4. **Jira updates:** with `jira.comment_cmd` set, a merge drafts a comment for you to edit and post (and a move, with `watch.merged_transition`). With `watch.dispatch_transition`, dispatching a ticket moves it straight away, say to In Progress. For twg, a transition is its id: `twg jira workitem transition --id SRE-123 -o json` lists them without moving anything.

### Run a ticket through stages

1. **Dispatch it as a group:** **Dispatch…** in Pipeline, or **Hand off to agents…** in Jira, defaults to **New group + orchestrator** with the workflow on. The `<ticket>-lead` starts the first stage (by default triage → research → blueprint → build → close).
2. **Approve each handoff** in the Inbox: read it, change the next stage's model in the box if you want, and **Approve**, or **Send back** with notes. The finished stage's agent stops; the lead starts the next.
3. **Long builds** write a checkpoint and continue in a fresh session once a session reaches `stages.build_checkpoint_tokens` or `stages.build_checkpoint_usd`.
4. **Models per stage:** `[stages.models]`, e.g. `triage = "claude-haiku-5-5"`.
5. **Review builds as they happen:** set `stages.review = true`, `stages.models.review` to the reviewer's model and, optionally, `stages.review_skill` to a SKILL.md. Each build agent then gets `<builder>-review` alongside it. The builder messages it when something works, and it replies with findings and diffs. It can't edit files itself, and it stops when its builder does.

### Read a ticket's timeline

**Timeline** in an agent's header, **timeline** on a group, or **Timeline** in Pipeline: everything that happened on the ticket, day by day, with totals at the top (runs and cost, questions asked and declined, decisions, wakes). Filter by runs, decisions and the Inbox, messages, status and reports, agents and wakes, Jira and PRs, stages, or retro, and click an event to see all of it. It's read from `ledger.jsonl` in the ticket's folder; **Show folder** opens it.

### Run a retro

1. **Start it:** **Start retro** in a ticket's timeline, or **Retro…** in Pipeline for several tickets, with a note if there's something to look at in particular. The picker shows when each ticket last had a retro. Tickets with nothing new since then are under **with nothing new since their retro**.
2. A retro agent reads the records and the numbers drovr works out (cost by stage and model, rework, dismissals, misses) and proposes changes to stage models, checkpoint limits, wake rules, the process and your skills, each with its evidence. It's told what earlier retros proposed and what you decided, so it doesn't bring back what you dismissed without new evidence.
3. **Decide:** under **Retros** in Pipeline, approve or dismiss each, or **Approve all**. Approving applies it: a config change keeps your file's comments and saves the previous one as `config.toml.before-retro`; a skill is written to `retro.skills_dir`. A retro you've decided all of folds away under **N finished retros**.

### Have an agent write a Confluence page

1. **Set it up once** in Settings (the twg templates are in the README under [Pages agents write](README.md#pages-agents-write)): `confluence.page_get_cmd`, `page_update_cmd` and `page_create_cmd`. Agents get `publish_confluence` from their next run.
2. **Tell the agent where:** the page to update (a blank page you made works), or a page or folder to create it under.
3. **Review it in the Inbox:** the diff against the live draft and the full page. **Save as draft in Confluence**, or **Send back** with notes. drovr reads the page again before saving; nothing is ever published, so you publish it in Confluence.

### Choose models

Claude Code agents use `defaults.model`, or your Claude Code default when it's empty; Codex agents use `codex.model`, or Codex's default. To pick another for one agent, fill in **Model** in **New agent** or **Dispatch**, or **Lead's model** in **New group**. Workers a lead starts without asking run on the lead's model; workers you approve (the **Spawn** box) and stages (the handoff box) default to their configured model, and you can change it there. Lookups use `agent_queries.model`, which is `haiku` by default. With the ticket workflow, each stage can have its own (`stages.models.<stage>`, or the box on a handoff), and so can the retro (`retro.model`). To use Opus for the work and Haiku for searching, set this in **Settings**:

```toml
[defaults]
model = "claude-opus-5-5"

[agent_queries]
model = "haiku"
```

## CLI reference

The `drovr` CLI does what the app does, from a shell, a herdr pane, or a script. It uses the same database, so the app shows whatever the CLI does. Put it on your PATH with **drovr → Install Command Line Tool…** (or Settings → Command line tool), which links `/usr/local/bin/drovr` to the app's bundled CLI and may ask for your password. From source, use `cargo install --path crates/drovr-cli`.

Agent and group names are lowercase letters, digits, `-` and `_`, starting with a letter. `new`, `adopt` and `group` lowercase the name and turn spaces into dashes, so `drovr new "Fix login" …` starts `fix-login`; the name it prints is the one to use afterwards.

| Command | What it does |
| --- | --- |
| `drovr ls` | List agents: group, role, state, cost, status |
| `drovr new <name> <cwd> <task…>` | Start a solo agent |
| `drovr group <name> <cwd> <goal…>` | Start a group with an orchestrator |
| `… --harness claude\|opencode\|codex` | With `new`, `group`, `dispatch` or `adopt`: the CLI that runs the agents (default `defaults.harness`). With `sessions`: whose sessions to list (default Claude Code) |
| `drovr dispatch <TICKET> [solo\|group] [cwd]` | Hand a Jira ticket to a new agent or a new group |
| `drovr dispatch <TICKET> to <group> [agent]` | Hand a Jira ticket to an existing group: its lead, or the agent you name |
| `drovr sessions` | List recent Claude Code sessions (`--harness opencode`: OpenCode's) |
| `drovr adopt <session-id> <name> [--fork] [task…]` | Start an agent from an existing Claude Code session, or an OpenCode one (`ses_…`) |
| `drovr review <pr-url>` | Start a read-only reviewer for a GitHub PR in its own worktree; prints `started <name> in <cwd>` |
| `drovr review poll` | Look now for PRs to review (see `[review]`) and list the new ones in PRs; prints `offered <owner>/<repo>#<n>: <title> (<source>)` for each, or `nothing new`; with nothing to watch it says `review polling is off: set review.slack_channels or review.github_teams` |
| `drovr review list` | List the PRs waiting on you, one per line: `new` or `reviewing`, `owner/repo#n`, the title, `(source)` and the reviewer's name; `No PRs waiting on you.` when there are none |
| `drovr review dismiss <pr-url>` | Hide a PR from PRs; its reviewer, worktree and branch are removed; prints `dismissed <url>` |
| `drovr prompt <agent> <text…>` | Send an agent a prompt (queued if it's busy) |
| `drovr stop <agent>` | Stop an agent's current run |
| `drovr resume <agent> [text…]` | Resume a stopped, failed or interrupted agent |
| `drovr rm <agent>` | Stop an agent and forget it; says if its worktree was kept |
| `drovr log <agent> [lines] [--pane]` | Show an agent's output. A herdr agent's is its session, as for a headless agent; `--pane` shows its pane's screen |
| `drovr attach <agent>` | Print a command that opens the agent's session in a terminal |
| `drovr inbox` | List everything waiting for you: decisions, commands, permission requests, worker requests, and with the ticket workflow, Watcher items, Jira drafts, handoffs and Confluence pages |
| `drovr answer <id> <answer…>` | Answer an item: a decision's answer; `allow` / `deny` a permission request; `Send to <agent>` a Watcher item; `Approve [model=<model>]` or `Send back: <notes>` a handoff; the text to post for a Jira draft; `approve` a Confluence page |
| `drovr reject <id> [reason…]` | Reject an inbox item |
| `drovr ran <id> [output…]` | Mark a proposed command as run by you; the agent gets the output or note |
| `drovr run <id> [local\|herdr]` | Run a proposed command; with `local`, the output goes back to the agent |
| `drovr msgs [group]` | Show the message bus |
| `drovr ask <question…>` | Ask a one-shot agent that uses your MCP servers |
| `drovr cleanup [--dry-run]` | Archive old plans and remove resolved Inbox items, per `reports.archive_after_days` and `inbox.keep_days`; `--dry-run` only lists what it would do |
| `drovr config` | Print where drovr reads its config and keeps its data, as JSON |
| `drovr remote [--test]` | Print the phone page's sign-in link and its QR code, and whether the page and notifications are on; `--test` sends a test notification |
| `drovr serve` | Serve the phone page and send notifications without the app (see `[remote]` in the config) |
| `drovr help` | Show this list |

Agents and run wrappers call four internal commands: `drovr mcp --agent <name>` is an agent's MCP server, `drovr hook finished <agent>` runs when a headless run exits, `drovr hook permission <agent>` is an OpenCode agent's permission prompt (its plugin passes the tool call on stdin and reads the answer), and `drovr status-line <agent>` is a herdr agent's Claude Code status line, which saves its cost.
