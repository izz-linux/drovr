//! Configuration: `~/.config/drovr/config.toml` over built-in defaults.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub defaults: Defaults,
    pub claude: Claude,
    pub codex: Codex,
    pub permissions: Permissions,
    pub orchestrator: Orchestrator,
    pub commands: Commands,
    pub herdr: Herdr,
    pub jira: Source,
    pub confluence: Source,
    pub slack: Source,
    pub datadog: Datadog,
    pub memory: Memory,
    pub agent_queries: AgentQueries,
    pub reports: Reports,
    pub terminal: Terminal,
    pub notifications: Notifications,
    pub remote: Remote,
    pub workflow: Workflow,
    pub watch: Watch,
    pub stages: Stages,
    pub retro: RetroConfig,
}

/// The retro you start from the timeline or Pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetroConfig {
    /// The retro agent's model; empty for the default.
    pub model: String,
    /// Where approved skill proposals are written.
    pub skills_dir: String,
}

impl Default for RetroConfig {
    fn default() -> Self {
        RetroConfig {
            model: String::new(),
            skills_dir: "~/.claude/skills".into(),
        }
    }
}

/// The ticket workflow's stages: each is an agent the group's lead spawns,
/// and you approve every handoff between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Stages {
    /// In order.
    pub order: Vec<String>,
    /// The model for each stage's agent; a stage that isn't listed (or is
    /// empty) uses the harness's default.
    pub models: std::collections::BTreeMap<String, String>,
    /// The build stage starts a fresh session from a checkpoint once its
    /// context passes this many tokens, or its session has cost this many
    /// dollars, whichever comes first. 0 turns that limit off.
    pub build_checkpoint_tokens: u64,
    pub build_checkpoint_usd: f64,
    /// Remove stopped stage agents when the last stage's handoff is
    /// approved. Their sessions and the record stay.
    pub tidy_on_close: bool,
}

impl Default for Stages {
    fn default() -> Self {
        Stages {
            order: ["triage", "research", "blueprint", "build", "close"]
                .map(String::from)
                .to_vec(),
            models: Default::default(),
            build_checkpoint_tokens: 150_000,
            build_checkpoint_usd: 15.0,
            tidy_on_close: true,
        }
    }
}

impl Stages {
    /// The stage after `stage`, if any.
    pub fn next(&self, stage: &str) -> Option<&str> {
        let i = self.order.iter().position(|s| s == stage)?;
        self.order.get(i + 1).map(String::as_str)
    }

    pub fn model(&self, stage: &str) -> &str {
        self.models.get(stage).map(String::as_str).unwrap_or("")
    }
}

/// The ticket workflow, as a whole: the record of each ticket's work and
/// its timeline, and what's built on them. Off, drovr works as it did
/// before: nothing is recorded, agents don't get `record_decision`, and
/// the app shows no timeline. Records already written stay on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    pub enabled: bool,
}

/// The watcher, part of the ticket workflow: it keeps the status of your
/// sprint's tickets (through the Jira `command` or `rest` provider) and of
/// their pull requests (through `gh`), without an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Watch {
    /// The tickets to follow.
    pub jql: String,
    /// Where to look for PRs: `owner/repo`, or `owner/*` for all of an
    /// owner's repos. A PR belongs to a ticket when its title has the key.
    pub github_repos: Vec<String>,
    pub jira_poll_secs: u64,
    pub pr_poll_secs: u64,
    /// The GitHub CLI.
    pub gh: String,
    /// What to do when something changes: `wake` the agent that owns the
    /// work, `ask` you in the Inbox first, or `ignore` it (it's still in the
    /// record).
    pub rules: Rules,
    /// When a ticket's PR merges, also draft moving the ticket with this
    /// transition (`{transition}` in `jira.transition_cmd`; for twg, its
    /// id); empty drafts only a comment.
    pub merged_transition: String,
    /// When you dispatch a ticket, move it with this transition (e.g. the
    /// one to In Progress) straight away; empty leaves it where it is.
    pub dispatch_transition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// A PR's checks finished and some failed.
    pub ci_failed: String,
    /// A reviewer requested changes.
    pub changes_requested: String,
    /// A new review or review comments, without a change request.
    pub new_review: String,
    /// The PR has merge conflicts.
    pub conflict: String,
    /// Approved, checks green, no conflicts: ready to merge.
    pub approved_green: String,
    pub merged: String,
    /// Closed without merging.
    pub pr_closed: String,
    /// The ticket's status, assignee or comments changed, or it left the sprint.
    pub jira_changed: String,
}

impl Default for Rules {
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        Rules {
            ci_failed: s("wake"),
            changes_requested: s("wake"),
            new_review: s("wake"),
            conflict: s("wake"),
            approved_green: s("ask"),
            merged: s("ask"),
            pr_closed: s("ask"),
            jira_changed: s("ask"),
        }
    }
}

impl Rules {
    /// The action for a trigger: `wake`, `ask` or `ignore`.
    pub fn action(&self, trigger: &str) -> &str {
        match trigger {
            "ci_failed" => &self.ci_failed,
            "changes_requested" => &self.changes_requested,
            "new_review" => &self.new_review,
            "conflict" => &self.conflict,
            "approved_green" => &self.approved_green,
            "merged" => &self.merged,
            "pr_closed" => &self.pr_closed,
            "jira_changed" => &self.jira_changed,
            _ => "ignore",
        }
    }
}

impl Default for Watch {
    fn default() -> Self {
        Watch {
            jql: "assignee = currentUser() AND sprint in openSprints() ORDER BY updated DESC"
                .into(),
            github_repos: vec![],
            jira_poll_secs: 300,
            pr_poll_secs: 120,
            gh: "gh".into(),
            rules: Rules::default(),
            merged_transition: String::new(),
            dispatch_transition: String::new(),
        }
    }
}

/// One auto memory for every Claude Code agent (see `memory.rs`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    /// The auto memory folder every Claude Code agent reads, wherever it
    /// works; leads and solo agents write it, and workers send theirs to
    /// their lead. Empty: Claude Code's own memory per project.
    pub dir: String,
}

/// Datadog through the `pup` CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Datadog {
    /// Agents query Datadog with pup: a read-only pup command runs without
    /// asking (drovr adds `--read-only`), and writes come to you.
    pub pup: bool,
    /// Your Datadog site when it isn't US1, e.g. "us5.datadoghq.com". Passed
    /// to agents as `DD_SITE`, which the app doesn't get from your shell.
    pub site: String,
}

/// macOS notifications from the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Notify when something new lands in the Inbox while drovr isn't the
    /// app you're using. The Dock badge shows the Inbox count either way.
    pub enabled: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications { enabled: true }
    }
}

/// The phone page: a small web server for the Inbox, agents, reports and
/// code review, and push notifications through ntfy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    /// Serve the page while the app runs (or `drovr serve` without it).
    pub enabled: bool,
    /// The address and port it listens on. `0.0.0.0` is every network the
    /// Mac is on; a Tailscale address keeps it to your tailnet.
    pub bind: String,
    /// The access token. Empty: drovr makes one and keeps it in
    /// `remote-token` in its data folder.
    pub token: String,
    /// How your phone reaches the page, e.g. `http://my-mac.tail1234.ts.net:7979`.
    /// Used for the link `drovr remote` prints and in push notifications.
    /// Empty: this Mac's `.local` name and the port.
    pub public_url: String,
    /// The ntfy topic push notifications go to; empty turns them off. Anyone
    /// who knows it can read them, so make it hard to guess.
    pub ntfy_topic: String,
    /// The ntfy server.
    pub ntfy_server: String,
    /// An environment variable holding an ntfy access token, for a protected
    /// topic or your own server.
    pub ntfy_token_env: String,
    /// Put the agent and the item's title in notifications. False sends
    /// only that something is waiting.
    pub push_details: bool,
    /// Serve HTTPS with this certificate: a PEM file with the certificate
    /// and its chain (Let's Encrypt's `fullchain.pem`). Set with `tls_key`.
    pub tls_cert: String,
    /// The certificate's private key, a PEM file (`privkey.pem`).
    pub tls_key: String,
}

impl Remote {
    /// The certificate and key files when HTTPS is on: both set, `~` expanded.
    pub fn tls_files(&self) -> anyhow::Result<Option<(PathBuf, PathBuf)>> {
        match (self.tls_cert.trim(), self.tls_key.trim()) {
            ("", "") => Ok(None),
            ("", _) | (_, "") => {
                anyhow::bail!("set both remote.tls_cert and remote.tls_key, or neither")
            }
            (cert, key) => Ok(Some((expand_tilde(cert).into(), expand_tilde(key).into()))),
        }
    }

    /// `https` when a certificate is set, else `http`.
    pub fn scheme(&self) -> &'static str {
        if self.tls_cert.trim().is_empty() || self.tls_key.trim().is_empty() {
            "http"
        } else {
            "https"
        }
    }
}

impl Default for Remote {
    fn default() -> Self {
        Remote {
            enabled: false,
            bind: "0.0.0.0:7979".into(),
            token: String::new(),
            public_url: String::new(),
            ntfy_topic: String::new(),
            ntfy_server: "https://ntfy.sh".into(),
            ntfy_token_env: String::new(),
            push_details: true,
            tls_cert: String::new(),
            tls_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// "headless" runs `claude -p` processes drovr owns; "herdr" runs
    /// interactive Claude Code sessions in herdr panes.
    pub backend: String,
    /// The CLI new agents run with: "claude" (Claude Code) or "codex"
    /// (experimental, headless only).
    pub harness: String,
    /// Directory new agents and groups start in when none is given.
    pub cwd: String,
    pub model: String,
    pub permission_mode: String,
    /// Give each agent its own git worktree so parallel agents don't collide.
    pub worktrees: bool,
    /// Re-prompt idle agents when a message or an answer arrives for them.
    pub auto_wake: bool,
    pub max_budget_usd: f64,
    /// Restart headless runs that a reboot or crash interrupted, as soon as
    /// drovr notices. Off: they wait for you as "interrupted".
    pub auto_resume: bool,
    /// Tools drovr agents may never use, e.g. ["Artifact"] so reports
    /// stay in drovr instead of being published elsewhere.
    pub disallowed_tools: Vec<String>,
    /// What agents call you ("Notes from Izz", "Izz answered #3"). Empty:
    /// "the human", so a shared config names nobody.
    pub your_name: String,
    /// Flag an agent in the app once its runs have cost more than this, in
    /// US dollars at API list prices. 0 turns it off.
    pub cost_warning_usd: f64,
    /// Copy text to the clipboard as soon as you select it in an agent's
    /// output. `terminal.copy_on_select` does the same for the terminal.
    pub copy_on_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Claude {
    pub command: String,
    pub extra_args: Vec<String>,
}

/// OpenAI's Codex CLI, for agents started with harness "codex"
/// (experimental). Codex has no permission prompt drovr can answer, so its
/// agents run in Codex's sandbox and bring anything outside it to the Inbox
/// with `propose_command`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Codex {
    pub command: String,
    /// Empty: Codex's own default model.
    pub model: String,
    /// read-only, workspace-write or danger-full-access.
    pub sandbox: String,
    pub extra_args: Vec<String>,
}

impl Default for Codex {
    fn default() -> Self {
        Codex {
            command: "codex".into(),
            model: String::new(),
            sandbox: "workspace-write".into(),
            extra_args: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// Tools headless agents may use without asking. A trailing `*` matches a
    /// prefix. Anything else becomes a permission request in the Inbox.
    /// `WebFetch` isn't here by default: with `Read` it would let an agent
    /// misled by a ticket or Slack message send your files anywhere.
    pub auto_allow: Vec<String>,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Orchestrator {
    /// Let orchestrators spawn workers without asking first.
    pub auto_spawn: bool,
    pub max_workers: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Commands {
    pub timeout_seconds: u64,
}

/// herdr CLI templates. `{name}` placeholders are shell-quoted on substitution,
/// so the templates can be adjusted to whatever herdr version is installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Herdr {
    pub command: String,
    /// With `--current`, drovr splits the pane it was started in, or else the
    /// focused pane, found with `list`.
    pub split: String,
    /// Opens the pane an agent starts in: `{cwd}` and the agent's `{name}`.
    pub agent_pane: String,
    /// Lists panes as JSON.
    pub list: String,
    /// Starts Claude Code in a pane. `{args}` is inserted already shell-quoted.
    pub start: String,
    pub run: String,
    pub prompt: String,
    pub read: String,
    pub stop: String,
    pub get: String,
    /// Shows a running agent's pane. `{target}` is the pane.
    pub focus: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// "agent" (ask Claude to use its MCP), "rest" (Atlassian / Slack API) or
    /// "command" (any CLI such as twg that prints JSON).
    pub provider: String,
    pub base_url: String,
    pub email: String,
    pub token_env: String,
    /// Command templates for the "command" provider: `{key}`, `{id}`, `{query}`.
    pub get_cmd: String,
    pub search_cmd: String,
    /// Jira only, for drafts you approve (ticket workflow): post a comment
    /// (`{key}`, `{id}`, `{body}`) and move a ticket (`{key}`,
    /// `{transition}`, `{fields}`).
    pub comment_cmd: String,
    pub transition_cmd: String,
    pub default_query: String,
    /// Confluence only: pages agents propose and you approve, saved as
    /// drafts. Read a page or folder (`{id}`: an ID or URL), update a page's
    /// draft (`{id}`, `{token}`, `{title}`, `{file}`) and create a draft page
    /// (`{space}`, `{parent}`, `{title}`, `{file}`).
    pub page_get_cmd: String,
    pub page_update_cmd: String,
    pub page_create_cmd: String,
    /// Jira only: the skill agents follow to write a ticket they create for
    /// their own task (the New agent dialog's "Create a Jira ticket").
    pub ticket_skill: String,
}

/// HTML reports for the human. A group's reports go in `<dir>/<group>/`,
/// a solo agent's in `<dir>` itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Reports {
    pub dir: String,
    /// When set, a ticket's work goes in `<tickets_dir>/<ticket>/` (the key
    /// lowercased): its reports, its record and its handoffs, whichever group
    /// or agent works it. Empty keeps them in `dir` with everything else.
    pub tickets_dir: String,
}

impl Default for Reports {
    fn default() -> Self {
        Reports {
            dir: "~/.claude/plans".into(),
            tickets_dir: String::new(),
        }
    }
}

/// The terminal panel under an agent's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    /// Shell to run; empty means `$SHELL`, else `/bin/zsh`.
    pub shell: String,
    /// CSS font family; empty means the app's monospace font. Set it to a
    /// Nerd Font (e.g. "MesloLGS NF") if your prompt uses its symbols.
    pub font_family: String,
    pub font_size: u32,
    /// Let the Option key send Meta (Esc+), like iTerm2's "Esc+" setting.
    pub option_as_meta: bool,
    /// Copy text to the clipboard as soon as you select it, like iTerm2's
    /// "Copy to pasteboard on selection".
    pub copy_on_select: bool,
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal {
            shell: String::new(),
            font_family: String::new(),
            font_size: 13,
            option_as_meta: false,
            copy_on_select: true,
        }
    }
}

impl Terminal {
    pub fn shell(&self) -> String {
        if !self.shell.trim().is_empty() {
            return expand_tilde(self.shell.trim());
        }
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/zsh".into())
    }
}

impl Reports {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(expand_tilde(&self.dir))
    }

    /// The tickets folder, when `tickets_dir` is set.
    pub fn tickets_path(&self) -> Option<PathBuf> {
        let d = self.tickets_dir.trim();
        (!d.is_empty()).then(|| PathBuf::from(expand_tilde(d)))
    }

    /// `ticket`'s folder in the tickets folder, when it's set.
    pub fn ticket_path(&self, ticket: &str) -> Option<PathBuf> {
        let ticket = ticket.trim();
        if ticket.is_empty() {
            return None;
        }
        self.tickets_path().map(|d| d.join(ticket.to_lowercase()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentQueries {
    /// Model for lookups. Fetching and formatting tickets doesn't need a big
    /// model, and a small one answers several times faster.
    pub model: String,
    pub timeout_seconds: u64,
    /// Extra tools lookups may use without asking, beyond the read-only MCP
    /// tools drovr recognises by name. A trailing `*` matches a prefix.
    pub auto_allow: Vec<String>,
    /// MCP servers whose read-only tools lookups may use, matched as part of
    /// the server's name (`atlassian` matches `claude_ai_Atlassian`). Empty:
    /// any server.
    pub servers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            defaults: Defaults::default(),
            claude: Claude::default(),
            codex: Codex::default(),
            permissions: Permissions::default(),
            orchestrator: Orchestrator::default(),
            commands: Commands::default(),
            herdr: Herdr::default(),
            jira: Source {
                token_env: "JIRA_API_TOKEN".into(),
                default_query:
                    "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC"
                        .into(),
                ..Source::default()
            },
            confluence: Source {
                token_env: "JIRA_API_TOKEN".into(),
                ..Source::default()
            },
            slack: Source {
                token_env: "SLACK_USER_TOKEN".into(),
                ..Source::default()
            },
            agent_queries: AgentQueries::default(),
            reports: Reports::default(),
            terminal: Terminal::default(),
            notifications: Notifications::default(),
            remote: Remote::default(),
            datadog: Datadog::default(),
            memory: Memory::default(),
            workflow: Workflow::default(),
            watch: Watch::default(),
            stages: Stages::default(),
            retro: RetroConfig::default(),
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            backend: "headless".into(),
            harness: "claude".into(),
            cwd: String::new(),
            model: String::new(),
            permission_mode: "acceptEdits".into(),
            worktrees: false,
            auto_wake: true,
            max_budget_usd: 0.0,
            disallowed_tools: vec![],
            auto_resume: false,
            your_name: String::new(),
            cost_warning_usd: 10.0,
            copy_on_select: true,
        }
    }
}

impl Default for Claude {
    fn default() -> Self {
        Claude {
            command: "claude".into(),
            extra_args: vec![],
        }
    }
}

impl Default for Permissions {
    fn default() -> Self {
        Permissions {
            auto_allow: [
                "Read",
                "Glob",
                "Grep",
                "LS",
                "TodoWrite",
                "WebSearch",
                "mcp__drovr__*",
            ]
            .map(String::from)
            .to_vec(),
            wait_seconds: 900,
        }
    }
}

impl Default for Orchestrator {
    fn default() -> Self {
        Orchestrator {
            auto_spawn: true,
            max_workers: 6,
        }
    }
}

impl Default for Commands {
    fn default() -> Self {
        Commands {
            timeout_seconds: 600,
        }
    }
}

impl Default for Herdr {
    fn default() -> Self {
        Herdr {
            command: "herdr".into(),
            split: "{herdr} pane split --current --direction right --cwd {cwd} --no-focus".into(),
            agent_pane: "{herdr} tab create --cwd {cwd} --label {name} --no-focus".into(),
            list: "{herdr} pane list".into(),
            start:
                "{herdr} agent start {name} --kind claude --pane {pane} --timeout 60000 -- {args}"
                    .into(),
            run: "{herdr} pane run {pane} {cmd}".into(),
            prompt: "{herdr} agent prompt {target} {text}".into(),
            read: "{herdr} agent read {target} --source recent-unwrapped --lines {lines}".into(),
            stop: "{herdr} pane close {pane}".into(),
            get: "{herdr} agent get {target}".into(),
            focus: "{herdr} agent focus {target}".into(),
        }
    }
}

impl Default for Source {
    fn default() -> Self {
        Source {
            provider: "agent".into(),
            base_url: String::new(),
            email: String::new(),
            token_env: String::new(),
            get_cmd: String::new(),
            search_cmd: String::new(),
            comment_cmd: String::new(),
            transition_cmd: String::new(),
            default_query: String::new(),
            page_get_cmd: String::new(),
            page_update_cmd: String::new(),
            page_create_cmd: String::new(),
            ticket_skill: String::new(),
        }
    }
}

impl Default for AgentQueries {
    fn default() -> Self {
        AgentQueries {
            model: "haiku".into(),
            timeout_seconds: 180,
            auto_allow: vec![],
            servers: vec![],
        }
    }
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn user_home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("."))
}

/// Where drovr keeps its database and agent run logs.
pub fn home() -> PathBuf {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(not(test))]
fn data_dir() -> PathBuf {
    env_path("DROVR_HOME").unwrap_or_else(|| user_home().join(".local/share/drovr"))
}

/// Unit tests stay out of the real data folder, even with `DROVR_HOME` set.
#[cfg(test)]
fn data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("drovr-test-home-{}", std::process::id()))
}

pub fn config_path() -> PathBuf {
    env_path("DROVR_CONFIG").unwrap_or_else(|| {
        env_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| user_home().join(".config"))
            .join("drovr/config.toml")
    })
}

pub fn parse(text: &str) -> anyhow::Result<Config> {
    Ok(toml::from_str(text)?)
}

pub fn to_toml(v: &serde_json::Value) -> anyhow::Result<String> {
    Ok(toml::to_string_pretty(v)?)
}

impl Config {
    pub fn load() -> anyhow::Result<Config> {
        let path = config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)?;
        parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
    }

    /// Whether `to` means you: `human` (how drovr stores it), `the_human`,
    /// `the human`, `the-human`, `me`, `user`, or your name, in any case.
    pub fn is_you(&self, to: &str) -> bool {
        let t = to
            .trim()
            .trim_start_matches('@')
            .to_lowercase()
            .replace(['_', '-'], " ");
        matches!(
            t.as_str(),
            "human" | "the human" | "me" | "user" | "the user"
        ) || t == self.your_name().to_lowercase()
    }

    /// How a stored sender or recipient reads: `human` becomes your name.
    pub fn display_name(&self, who: &str) -> String {
        if who == "human" {
            self.your_name()
        } else {
            who.to_string()
        }
    }

    /// The name agents use for you.
    pub fn your_name(&self) -> String {
        let configured = self.defaults.your_name.trim();
        if !configured.is_empty() {
            return configured.to_string();
        }
        "the human".into()
    }

    pub fn default_cwd(&self) -> String {
        if self.defaults.cwd.is_empty() {
            user_home().display().to_string()
        } else {
            expand_tilde(&self.defaults.cwd)
        }
    }
}

/// `~` and `~/…` are your home folder. `~name` is left alone: it means
/// another user's home, not a folder in yours.
pub fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", user_home().display())
        }
        _ => path.to_string(),
    }
}

/// The tool agents call to bring you a decision: `ask_izz` for "Izz", or
/// `ask_human` when drovr doesn't know your name.
pub fn ask_tool(you: &str) -> String {
    let mut slug = String::new();
    for c in you.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug: String = slug.trim_matches('_').chars().take(40).collect();
    if you == "the human" || slug.is_empty() {
        "ask_human".into()
    } else {
        format!("ask_{slug}")
    }
}

/// Text agents read, with "the human" as your name and `ask_human` as your
/// ask tool. Unchanged when drovr doesn't know your name.
pub fn personalize(text: &str, you: &str) -> String {
    if you == "the human" {
        return text.to_string();
    }
    let handle = ask_tool(you).trim_start_matches("ask_").to_string();
    text.replace("ask_human", &ask_tool(you))
        .replace(
            "`human` to message the human you work for",
            &format!("`{handle}` to message {you}"),
        )
        .replace("a human supervises", &format!("{you} supervises"))
        .replace("the human's", &format!("{you}'s"))
        .replace("the human", you)
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn tests_never_use_the_real_data_folder() {
        let home = super::home();
        assert!(home.starts_with(std::env::temp_dir()), "{home:?}");
    }

    #[test]
    fn recognises_the_human() {
        let mut cfg = Config::default();
        // Unset, drovr names nobody.
        assert_eq!(cfg.your_name(), "the human");
        assert_eq!(super::ask_tool(&cfg.your_name()), "ask_human");
        cfg.defaults.your_name = "Izz".into();
        for to in [
            "human",
            "the_human",
            "the human",
            "The-Human",
            "@human",
            "me",
            "user",
            "Izz",
            "izz",
        ] {
            assert!(cfg.is_you(to), "{to}");
        }
        for to in ["humane", "pay-api-lead", "@group", "orchestrator"] {
            assert!(!cfg.is_you(to), "{to}");
        }
        assert_eq!(cfg.display_name("human"), "Izz");
        assert_eq!(cfg.display_name("pay-api-lead"), "pay-api-lead");
        assert!(cfg.is_you("izz"));
    }

    #[test]
    fn names_the_ask_tool_after_you() {
        use super::{ask_tool, personalize};
        assert_eq!(ask_tool("Izz"), "ask_izz");
        assert_eq!(ask_tool("Mary Jo"), "ask_mary_jo");
        assert_eq!(ask_tool("José"), "ask_jos");
        assert_eq!(ask_tool("the human"), "ask_human");
        assert_eq!(ask_tool("  "), "ask_human");
        assert_eq!(
            personalize("Use `ask_human` when the human must decide, or `human` to message the human you work for.", "Izz"),
            "Use `ask_izz` when Izz must decide, or `izz` to message Izz."
        );
        assert_eq!(
            personalize("ask_human for the human", "the human"),
            "ask_human for the human"
        );
    }
}
