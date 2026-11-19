# PR review dispatcher

## Why

PRs that need SRE's review show up in two places:
- Links posted in certain Slack channels. This is the main source.
- GitHub review requests to the SRE team, from CODEOWNERS.

Today the human finds them by hand and starts a review by hand. drovr should notice them and offer a reviewer. A yes should start an agent that runs the human's review skill on the review model and reports back without posting anything. The agent then stays available, so the human can tell it to post.

## Goals

1. **Config,** with everything the human's to set. Empty or zero means off.
   ```toml
   [review]
   slack_channels  = ["#sre-reviews"]   # watch these for GitHub PR links
   github_teams    = ["acme/sre"]       # PRs requesting this team's review
   poll_secs       = 600                # how often to look; 0 = off
   auto_dispatch   = false              # start a reviewer without asking
   remove_when_closed = true            # remove a reviewer (and its worktree) once its PR merges or closes
   repos_dir       = ""                 # where local clones live (cloned there if missing); empty = defaults.cwd
   ```
   `stages.review_skill` and `stages.models.review` are reused for the skill and the model.
2. **Finding PRs.** Each time a poll is due, from the app's background loop:
   - **Slack:** one lookup for all channels, for messages since the last poll (the first poll looks back 1 day), through the configured Slack provider:
     - `rest`: `search.messages` per channel
     - `command`: `search_cmd` per channel
     - `agent`: one `claude -p` with the Slack MCP for all channels
     PR URLs are taken from the message text: `https://github.com/<owner>/<repo>/pull/<n>`.
   - **GitHub:** `gh search prs --review-requested=<org/team> --state=open --json url,title,isDraft,author` for each team.
   - **Enrichment and filtering:** each new URL is looked up with `gh pr view <url> --json url,title,state,isDraft,author`. Only open, non-draft PRs whose author isn't the gh user (`gh api user --jq .login`, cached for the poll) go on.
   - **Dedupe:** each PR is offered **once ever**, keyed by its URL. Seen URLs live in `<home>/review.json` with when they were seen, and entries older than 90 days are dropped. The poll's `since` is kept there too.
   - A failure in one source logs and doesn't stop the other.
3. **Offering it.** A new Inbox kind, `review`:
   - Agent `drovr`. Title "Review <owner>/<repo>#<n>: <title>". The body has the URL, the author, the source (`#channel` with the Slack permalink, or `team acme/sre`), and the poster for Slack.
   - Options: **Dispatch reviewer** and **Dismiss**.
   - With `auto_dispatch`, the reviewer starts at once, and an informational item (kind `review`, already answered, "Reviewer <name> started") records it.
4. **The reviewer.** On Dispatch, a solo agent starts:
   - **Name:** `review-<repo>-<n>`, made unique the usual way.
   - **Model:** `stages.models.review`, or the default if that's unset. **Harness:** `defaults.harness`.
   - **Its own worktree of the PR,** made by drovr before the agent starts, whatever `defaults.worktrees` says:
     - The clone is `<repos_dir>/<repo>`. If it's missing, drovr runs `gh repo clone <owner>/<repo> <repos_dir>/<repo>` first.
     - drovr runs `git fetch origin pull/<n>/head`, then `git worktree add <worktrees>/<name> -b drovr/<name> FETCH_HEAD`, the same folder and branch scheme as other agents' worktrees.
     - The agent's cwd is that worktree, at the PR's head commit. See Cleaning up below.
     - If the clone or fetch fails, the Inbox item stays pending with the error, and no agent starts.
   - **Task** (a prompt in `prompts.rs`):
     - review PR `<url>` following the review skill at `<stages.review_skill>` (if set; otherwise a thorough code review)
     - **read-only:** don't push, comment, approve or request changes on GitHub
     - publish the findings with `publish_report` (title "Review <owner>/<repo>#<n>")
     - send the human a short summary (`send_message` to `human`) with the verdict and the top findings
     - then stop and wait: the human may ask it to post the review to the PR, and only then does it use `gh pr review`/`gh pr comment` with what they approve
   - It isn't stopped when it finishes. It stays in Agents like any solo agent, so the human can message it.
   - **Cleaning up.** A reviewer is marked as one: role `pr-reviewer`, with the PR URL kept on the agent, e.g. in its ticket field or a payload; the plan picks the existing column that fits. When the human is done:
     - **Removing it** (the existing remove button or `drovr rm <name>`) also force-removes its worktree (`git worktree remove --force`), deletes its `drovr/<name>` branch (`git branch -D`), and prunes worktrees. Its scratch checkout is never kept, unlike other agents'.
     - **Automatically,** with `review.remove_when_closed = true` (the default), drovr removes an idle reviewer once its PR is merged or closed. That's checked on the same poll, with `gh pr view <url> --json state`. A reviewer that's working is left until its next poll after it goes idle.
     - The clone in `repos_dir` is the human's and is never removed.
5. **Running it by hand:**
   - `drovr review <pr-url>` dispatches a reviewer for any PR now.
   - `drovr review poll` runs a poll now and prints what it found.

## Non-goals

- Posting reviews automatically.
- Re-offering a PR after new pushes.
- Watching DMs, threads beyond the channel's top-level messages and replies the search returns, or links that aren't GitHub ones.
- A Settings UI for `[review]`. It's edited in the config like other sections.

## Tests

- **URL extraction:** a plain URL, `<url|text>` Slack link formatting, multiple links, `/pull/12/files`, and non-PR GitHub URLs ignored.
- **Poll** (with fake sources injected, as `watch.rs` does with `fake_watch` or similar):
  - a new PR gives one `review` item
  - the same URL on the next poll gives none
  - drafts, closed PRs and the human's own PRs are skipped
  - a failing Slack source still lets GitHub items through
  - `poll_secs = 0` does nothing
  - `auto_dispatch` starts an agent and records an answered item
- **Answering:**
  - Dispatch starts a solo agent with the review model, cwd = a new worktree at the PR head (test with a local bare "origin" repo that has a `refs/pull/7/head` ref), and a task containing the URL, the skill path and "read-only".
  - A missing clone runs the clone command; a failed fetch leaves the item pending and starts no agent.
  - Dismiss starts nothing.
- **Cleanup:**
  - removing a reviewer deletes its worktree, even with uncommitted files, and its branch
  - removing a normal agent is unchanged: an uncommitted worktree is kept, and the branch is always kept
  - with `remove_when_closed`, a closed PR's idle reviewer is removed
  - a working reviewer, or `remove_when_closed = false`, keeps it
- **State:** `review.json` drops entries older than 90 days, and a corrupt file means empty state.
