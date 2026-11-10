//! Agent lifecycle: start, prompt, stop, wake, and read output.
//!
//! Two backends:
//! - `headless`: each prompt is a detached `claude -p --output-format stream-json`
//!   run that resumes the agent's session. Runs survive the app closing; a
//!   wrapper calls `drovr hook finished <agent>` when one exits.
//! - `herdr`: an interactive Claude Code session in a herdr pane, driven
//!   through the herdr CLI (templates in config).

use crate::config::{expand_tilde, home, Config};
use crate::harness::{self, Costs, Event, Harness, Run, Session};
use crate::ledger;
use crate::prompts;
use crate::shell::{self, quote};
use crate::store::{Agent, Item, Store};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Auto-wakes allowed per agent in ten minutes, so two agents messaging each
/// other can't loop forever on your API bill.
const WAKE_LIMIT: usize = 15;

/// Longest prompt typed straight into a herdr pane; longer ones go via a file.
const HERDR_INLINE_MAX: usize = 600;

/// How long a new herdr pane's shell gets to reach its prompt.
const HERDR_READY_WAIT: Duration = Duration::from_secs(15);
const HERDR_READY_POLL: Duration = Duration::from_millis(500);

/// herdr can still report an agent as idle just after it's been prompted, so
/// a run this new, in seconds, isn't over yet.
const HERDR_RUN_SETTLE: f64 = 5.0;

/// What drovr has counted of a herdr agent's cost.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct HerdrRun {
    session: String,
    total: f64,
    run_cost: f64,
    started: f64,
}

/// Save what an agent's status line was given, for `refresh_herdr` to read.
pub fn save_status_line(name: &str, status: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("no agent named {name}");
    }
    let dir = home().join("agents").join(name);
    std::fs::create_dir_all(&dir)?;
    let partial = dir.join("status-line.json.tmp");
    std::fs::write(&partial, status)?;
    std::fs::rename(partial, dir.join("status-line.json"))?;
    Ok(())
}

pub struct Engine {
    pub store: Store,
    pub cfg: Config,
    cli: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewAgent {
    pub name: String,
    pub group: String,
    pub role: String,
    pub backend: String,
    /// `claude` or `codex`; empty for `defaults.harness`.
    pub harness: String,
    pub cwd: String,
    pub ticket: String,
    pub task: String,
    /// Start from this existing Claude Code session instead of a new one.
    pub resume_session: String,
    /// With `resume_session`: continue in a copy (`--fork-session`) and leave
    /// the original untouched.
    pub fork: bool,
    /// Its stage in the ticket workflow; empty outside it.
    pub stage: String,
    /// The model to run on; empty for the configured default (for a
    /// stage agent, `stages.models.<stage>`).
    pub model: String,
    /// With no ticket: the agent first writes and creates one for its task
    /// (with `jira.ticket_skill`), then takes it with `set_ticket`.
    pub create_ticket: bool,
    /// What you want in that ticket: "a bug, assigned to me, in the current
    /// sprint".
    pub ticket_notes: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewGroup {
    pub name: String,
    /// What the group is for. The orchestrator's task, when there is one.
    pub goal: String,
    pub ticket: String,
    pub cwd: String,
    pub backend: String,
    /// The orchestrator's harness; empty for `defaults.harness`.
    pub harness: String,
    /// Existing (or new) subfolder of the plans folder; defaults to `name`.
    pub plans_dir: String,
    /// Start a `<name>-lead` orchestrator.
    pub lead: bool,
    /// The orchestrator's model; empty for the harness's default.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    /// run | prompt | text | tool | result | final | error | raw
    pub kind: String,
    pub text: String,
}

/// The model a new agent runs on: the one picked for it, else its stage's
/// (`stages.models`), else empty for the harness's default.
fn agent_model(cfg: &Config, picked: &str, stage: &str) -> String {
    match (picked.trim(), stage) {
        ("", "") => String::new(),
        ("", stage) => cfg.stages.model(stage).to_string(),
        (m, _) => m.to_string(),
    }
}

/// The model a lead's new worker asks for. One the lead names wins. A worker
/// that starts without your approval (`orchestrator.auto_spawn`) runs on the
/// lead's model; a stage agent, or one you approve in the Inbox, gets its
/// default unless you pick one there.
fn worker_model(lead: &Agent, auto_spawn: bool, stage: &str, asked: &str) -> String {
    let asked = asked.trim();
    if !asked.is_empty() {
        asked.to_string()
    } else if auto_spawn && stage.is_empty() {
        lead.model.clone()
    } else {
        String::new()
    }
}

/// The model in an approval like "Approve model=<name>", if one was typed.
pub(crate) fn picked_model(answer: &str) -> Option<String> {
    answer
        .split_once("model=")
        .map(|(_, m)| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Find the `drovr` CLI that agents' MCP configs and run wrappers call.
pub fn locate_cli() -> PathBuf {
    if let Some(p) = std::env::var_os("DROVR_BIN") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().is_some_and(|n| n == "drovr") {
            return exe;
        }
        if let Some(dir) = exe.parent() {
            for candidate in ["drovr", "drovr-cli"] {
                let p = dir.join(candidate);
                if p.exists() {
                    return p;
                }
            }
        }
    }
    PathBuf::from("drovr")
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A typed name as drovr stores it: macOS capitalizes the first letter and
/// people type spaces, so lowercase it and turn runs of spaces into one `-`.
/// Anything else is left for `valid_name` to reject, not guessed at.
pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
}

pub fn slug(text: &str) -> String {
    let s: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let s: String = s.chars().take(30).collect();
    if s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s
    } else {
        format!("a-{s}")
    }
}

/// Inbox commands running in this process, so one can't start twice.
static RUNNING_COMMANDS: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

struct RunningCommand(i64);

impl RunningCommand {
    fn claim(id: i64) -> Result<RunningCommand> {
        let mut running = RUNNING_COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&id) {
            bail!("command #{id} is already running");
        }
        running.push(id);
        Ok(RunningCommand(id))
    }
}

impl Drop for RunningCommand {
    fn drop(&mut self) {
        let mut running = RUNNING_COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|r| *r != self.0);
    }
}

fn pid_alive(pid: i64) -> bool {
    pid > 0 && unsafe { libc::kill(pid as i32, 0) } == 0
}

/// Whether `pid` is still this agent's run wrapper. After a reboot, process
/// IDs are reused, so a live pid alone could be some unrelated process.
fn run_alive(pid: i64, agent: &str) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&format!("hook finished {agent}")),
        // Can't tell; trust the pid.
        Err(_) => true,
    }
}

/// Wait up to 5 seconds for a stopped run's process group to exit, then
/// kill what's left, so the next run doesn't share its session with it.
fn wait_for_group(pid: i64) {
    let gone = || unsafe { libc::killpg(pid as i32, 0) } != 0;
    for _ in 0..50 {
        if gone() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
}

const RESUME_TEXT: &str = "[drovr] Your previous run was interrupted (drovr or the machine restarted). Pick up where you left off.";

/// Search a JSON value for the first string under any of `keys`.
pub fn find_key(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(s) = map.get(*k).and_then(|x| {
                    x.as_str()
                        .map(String::from)
                        .or_else(|| (x.is_number()).then(|| x.to_string()))
                }) {
                    return Some(s);
                }
            }
            map.values().find_map(|x| find_key(x, keys))
        }
        Value::Array(items) => items.iter().find_map(|x| find_key(x, keys)),
        _ => None,
    }
}

fn git_in(dir: &Path, args: &str) -> Result<shell::Output> {
    shell::sh(
        &format!("git {args}"),
        Some(dir),
        None,
        Duration::from_secs(60),
    )
}

/// The repo's shared `.git` folder, the same for all its worktrees.
fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = git_in(dir, "rev-parse --path-format=absolute --git-common-dir").ok()?;
    out.ok()
        .then(|| PathBuf::from(out.stdout.trim()))
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// A worktree for agent `name` in `<root>/<name>`, on branch `drovr/<name>`.
/// An agent removed and started again under the same name picks up its old
/// branch, or its old worktree if that's still there. Outside a git repo the
/// agent just works in `cwd`.
fn make_worktree(root: &Path, cwd: &str, name: &str) -> Result<String> {
    let Some(repo) = git_common_dir(Path::new(cwd)) else {
        return Ok(cwd.to_string());
    };
    let dir = root.join(name);
    let shown = dir.display().to_string();
    if dir.exists() {
        if git_common_dir(&dir).as_ref() == Some(&repo) {
            return Ok(shown);
        }
        bail!("{shown} already exists and isn't a worktree of this repo; remove it or pick another name");
    }
    // Forget worktrees whose folders were deleted, so their branch is free.
    let _ = git_in(Path::new(cwd), "worktree prune");
    let branch = format!("drovr/{name}");
    let has_branch = git_in(
        Path::new(cwd),
        &format!(
            "rev-parse --verify --quiet {}",
            quote(&format!("refs/heads/{branch}"))
        ),
    )?
    .ok();
    let args = if has_branch {
        format!("worktree add {} {}", quote(&shown), quote(&branch))
    } else {
        format!("worktree add -b {} {}", quote(&branch), quote(&shown))
    };
    git_in(Path::new(cwd), &args)?.into_stdout("git worktree add")?;
    Ok(shown)
}

/// Remove an agent's worktree if it has nothing uncommitted (git refuses
/// otherwise). The branch stays either way. Returns what was left behind.
fn remove_worktree(dir: &Path, name: &str) -> String {
    let Some(main) = git_common_dir(dir).and_then(|c| c.parent().map(Path::to_path_buf)) else {
        return String::new();
    };
    let removed = git_in(
        &main,
        &format!("worktree remove {}", quote(&dir.display().to_string())),
    )
    .is_ok_and(|o| o.ok());
    if removed {
        format!("Its worktree was removed; branch drovr/{name} keeps its commits.")
    } else {
        format!(
            "Its worktree {} has uncommitted changes, so it was kept, with branch drovr/{name}.",
            dir.display()
        )
    }
}

/// A worker's name: `<group>-<name>`, cut to fit the 40-character limit.
fn worker_name(group: &str, name: &str) -> String {
    let base = slug(name);
    let full = if base.starts_with(&format!("{group}-")) {
        base
    } else {
        format!("{group}-{base}")
    };
    full.chars()
        .take(40)
        .collect::<String>()
        .trim_end_matches(['-', '_'])
        .to_string()
}

/// `v` as it is, or as a shortened JSON string when it's longer than `max`.
pub(crate) fn clip(v: &Value, max: usize) -> Value {
    let text = v.to_string();
    if text.len() <= max {
        v.clone()
    } else {
        Value::String(truncate(&text, max))
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}… ({} chars)",
            s.chars().take(max).collect::<String>(),
            s.chars().count()
        )
    }
}

impl Engine {
    pub fn open() -> Result<Engine> {
        Ok(Engine {
            store: Store::open_default()?,
            cfg: Config::load()?,
            cli: locate_cli(),
        })
    }

    pub fn with(store: Store, cfg: Config, cli: PathBuf) -> Engine {
        Engine { store, cfg, cli }
    }

    pub fn agent(&self, name: &str) -> Result<Agent> {
        self.store
            .agent(name)?
            .ok_or_else(|| anyhow!("no agent named {name}"))
    }

    /// Where to read what an agent did: its Claude Code session, and the
    /// logs of drovr's headless runs.
    pub fn transcripts(&self, agent: &Agent) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = harness::of(agent)
            .session_file(&agent.session_id)
            .into_iter()
            .collect();
        let runs = home().join("agents").join(&agent.name).join("runs");
        out.extend(
            (1..=agent.runs)
                .map(|r| runs.join(format!("{r}.jsonl")))
                .filter(|p| p.is_file()),
        );
        out
    }

    /// Where a terminal for this agent should open: the repo or worktree it
    /// worked in most recently, else its working directory.
    pub fn workspace(&self, name: &str) -> Result<String> {
        let a = self.agent(name)?;
        Ok(crate::review::roots(&a.cwd, &self.transcripts(&a))
            .into_iter()
            .next()
            .unwrap_or(a.cwd))
    }

    /// The git repos an agent worked in, with their changes.
    pub fn review(&self, name: &str) -> Result<Vec<crate::review::Repo>> {
        let a = self.agent(name)?;
        // The repo holding the plans folder (often ~/.claude) is every
        // agent's: show only the part this one worked in.
        let shared = [
            self.cfg.reports.path(),
            crate::config::user_home().join(".claude"),
        ];
        Ok(crate::review::repos(&a.cwd, &self.transcripts(&a), &shared))
    }

    /// What an agent's headless runs cost, from their logs. Only Claude Code
    /// reports a cost; Codex reports tokens.
    fn total_cost(&self, agent: &Agent) -> f64 {
        let harness = harness::of(agent);
        let dir = home().join("agents").join(&agent.name).join("runs");
        let mut costs = Costs::default();
        (1..=agent.runs)
            .filter_map(|r| std::fs::read_to_string(dir.join(format!("{r}.jsonl"))).ok())
            .filter_map(|log| {
                log.lines()
                    .rev()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .find_map(|v| match harness.parse(&v) {
                        Event::End(end) => Some(end),
                        _ => None,
                    })
            })
            .map(|end| harness.run_cost(&end, &mut costs).unwrap_or(0.0))
            .sum()
    }

    pub(crate) fn agent_dir(&self, name: &str) -> PathBuf {
        let dir = home().join("agents").join(name);
        let _ = std::fs::create_dir_all(dir.join("runs"));
        dir
    }

    fn mcp_config(&self, name: &str) -> Result<PathBuf> {
        let path = self.agent_dir(name).join("mcp.json");
        let cfg = json!({
            "mcpServers": {
                "drovr": {
                    "type": "stdio",
                    "command": self.cli.display().to_string(),
                    "args": ["mcp", "--agent", name],
                    "env": { "DROVR_HOME": home().display().to_string() }
                }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
        Ok(path)
    }

    /// Whether the agent works a ticket in stages: the workflow is on and its
    /// group was created with it on.
    pub(crate) fn staged(&self, agent: &Agent) -> bool {
        self.cfg.workflow.enabled
            && !agent.group_name.is_empty()
            && self
                .store
                .group(&agent.group_name)
                .ok()
                .flatten()
                .is_some_and(|g| g.workflow)
    }

    fn system_prompt_file(&self, agent: &Agent) -> Result<PathBuf> {
        let group = if agent.group_name.is_empty() {
            None
        } else {
            self.store.group(&agent.group_name)?
        };
        let peers = if agent.group_name.is_empty() {
            vec![]
        } else {
            self.store.agents(Some(&agent.group_name))?
        };
        let plans_dir = match &group {
            Some(g) => self
                .cfg
                .reports
                .path()
                .join(g.plans_folder())
                .display()
                .to_string(),
            None => String::new(),
        };
        let mut text = prompts::system_prompt(
            agent,
            group.as_ref(),
            &peers,
            self.cfg.orchestrator.max_workers,
            &plans_dir,
            &self.cfg.your_name(),
            self.cfg
                .workflow
                .enabled
                .then_some(self.cfg.stages.order.as_slice()),
        );
        if self.cfg.datadog.pup {
            text += &crate::config::personalize(crate::datadog::PROMPT, &self.cfg.your_name());
        }
        // Only Claude Code has auto memory.
        if harness::of(agent).name() == "claude" {
            text += &crate::config::personalize(
                &crate::memory::prompt(&self.cfg, agent),
                &self.cfg.your_name(),
            );
        }
        let path = self.agent_dir(&agent.name).join("system.md");
        std::fs::write(&path, text)?;
        Ok(path)
    }

    fn make_worktree(&self, cwd: &str, name: &str) -> Result<String> {
        make_worktree(&home().join("worktrees"), cwd, name)
    }

    // -- lifecycle ------------------------------------------------------------

    pub fn start_agent(&self, mut spec: NewAgent) -> Result<Agent> {
        spec.name = normalize_name(&spec.name);
        if !valid_name(&spec.name) {
            bail!("agent names are up to 40 lowercase letters, digits, - and _, starting with a letter (got {:?})", spec.name);
        }
        if self.store.agent(&spec.name)?.is_some() {
            bail!("an agent named {} already exists", spec.name);
        }
        if !spec.group.is_empty() && self.store.group(&spec.group)?.is_none() {
            bail!("there's no group named {}", spec.group);
        }
        let backend = if spec.backend.is_empty() {
            self.cfg.defaults.backend.clone()
        } else {
            spec.backend.clone()
        };
        let harness = harness::named(if spec.harness.is_empty() {
            &self.cfg.defaults.harness
        } else {
            &spec.harness
        })?;
        if backend == "herdr" && !harness.runs_in_herdr() {
            bail!(
                "{} agents run headless for now, not in herdr panes",
                harness.label()
            );
        }
        let resumed = match spec.resume_session.trim() {
            "" => None,
            id => Some(harness.find_session(id)?),
        };
        // `claude --resume` only finds a session from the directory it ran in.
        let mut cwd = match (&resumed, spec.cwd.is_empty()) {
            (Some(s), true) => s.cwd.clone(),
            (_, true) => self.cfg.default_cwd(),
            (_, false) => expand_tilde(&spec.cwd),
        };
        if !Path::new(&cwd).is_dir() {
            bail!("working directory {cwd} does not exist");
        }
        // A retro works in its own folder under the plans, never a worktree.
        if self.cfg.defaults.worktrees && resumed.is_none() && spec.stage != "retro" {
            cwd = self.make_worktree(&cwd, &spec.name)?;
        }
        let agent = Agent {
            name: spec.name.clone(),
            group_name: spec.group.clone(),
            role: match (spec.role.as_str(), spec.group.is_empty()) {
                ("", true) => "solo".into(),
                ("", false) => "worker".into(),
                (role, _) => role.into(),
            },
            backend,
            harness: harness.name().into(),
            run_state: "idle".into(),
            cwd,
            // Codex names the session when the first run starts.
            session_id: match &resumed {
                Some(s) => s.id.clone(),
                None if harness.picks_session_id() => uuid::Uuid::new_v4().to_string(),
                None => String::new(),
            },
            ticket: spec.ticket.clone(),
            task: spec.task.clone(),
            stage: spec.stage.clone(),
            model: agent_model(&self.cfg, &spec.model, &spec.stage),
            ..Agent::default()
        };
        self.store.add_agent(&agent)?;
        self.record(
            &agent.name,
            "agent_started",
            json!({
                "backend": agent.backend,
                "harness": agent.harness,
                "stage": agent.stage,
                "model": harness::model_of(harness, &agent, &self.cfg),
                "cwd": agent.cwd,
                "task": truncate(&spec.task, 4000),
                "resumed_session": resumed.as_ref().map(|s| s.id.clone()),
            }),
        );
        if resumed.is_some() {
            // Marks the session as existing; "fork" makes the first run copy it.
            std::fs::write(
                self.agent_dir(&agent.name).join("resumed"),
                if spec.fork { "fork" } else { "same" },
            )?;
        }
        let task = match (&resumed, spec.task.trim().is_empty()) {
            (Some(_), true) => format!(
                "[drovr] You're now running inside drovr as agent `{}`, with drovr's MCP tools for coordinating with other agents and with me. Continue the work from this conversation.",
                agent.name
            ),
            _ => spec.task.clone(),
        };
        let task = if spec.create_ticket && agent.ticket.trim().is_empty() {
            format!("{}{task}", self.ticket_first(&spec.ticket_notes))
        } else {
            task
        };
        if agent.backend == "herdr" {
            if let Err(e) = self.start_herdr(&Agent {
                task: task.clone(),
                ..agent.clone()
            }) {
                self.store.set_agent(&agent.name, "run_state", "error")?;
                self.store
                    .set_agent(&agent.name, "status", &format!("herdr start failed: {e}"))?;
                return Err(e);
            }
        } else {
            self.prompt(&agent.name, &task)?;
        }
        self.agent(&agent.name)
    }

    /// The start of a task whose agent creates its own Jira ticket first.
    fn ticket_first(&self, notes: &str) -> String {
        let how = match self.cfg.jira.ticket_skill.trim() {
            "" => "with a clear summary, a description and acceptance criteria".to_string(),
            skill => format!("following the ticket-writing skill at `{skill}` (read it first)"),
        };
        let notes = match notes.trim() {
            "" => String::new(),
            n => format!(" {} asks for: {n}.", capitalize(&self.cfg.your_name())),
        };
        format!(
            "## First: a Jira ticket for this\n\n\
             This work has no Jira ticket yet. Before starting it, write one {how}.{notes} \
             Create it in Jira (for example with `twg jira workitem create`), then call \
             `set_ticket` with its key: drovr records your work under it from then on. \
             Then do the task.\n\n## The task\n\n"
        )
    }

    /// Take a ticket for an agent's work, say one it just created: the
    /// agent's own, and its group's when it leads one that has none.
    pub fn set_ticket(&self, me: &Agent, key: &str) -> Result<String> {
        let key = match ledger::tickets_in(key).as_slice() {
            [k] => k.clone(),
            _ => bail!("give one ticket key, like SRE-123 (got {key:?})"),
        };
        self.store.set_agent(&me.name, "ticket", &key)?;
        if me.role == "orchestrator" {
            if let Some(g) = self.store.group(&me.group_name)? {
                if g.ticket.trim().is_empty() {
                    self.store.set_group_ticket(&g.name, &key)?;
                }
            }
        }
        self.record(
            &me.name,
            "ticket_set",
            json!({ "ticket": key, "before": me.ticket }),
        );
        Ok(format!("your ticket is {key}"))
    }

    /// "fork" while the first run should copy a resumed session, "same" when
    /// it continues it, None for a session drovr created.
    fn resume_mode(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.agent_dir(name).join("resumed"))
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Send text to an agent. Starts a run if it's idle; if it's busy the text
    /// is queued as a message from the human, which it reads (or is woken with).
    pub fn prompt(&self, name: &str, text: &str) -> Result<String> {
        let agent = self.agent(name)?;
        if agent.backend == "herdr" {
            // After a checkpoint, the run starts a fresh session.
            if let Some(fresh) = self.take_fresh(&agent)? {
                self.herdr_close_pane(&agent);
                self.store.set_agent(name, "pane", "")?;
                self.herdr_open_session(&fresh, Session::New, Some(text))?;
                return Ok("started".into());
            }
            self.herdr_prompt(&agent, text)?;
            self.record(
                name,
                "herdr_prompt",
                json!({ "prompt": truncate(text, 4000) }),
            );
            self.herdr_begin_run(name, "resume", text)?;
            return Ok("sent".into());
        }
        match self.store.claim_run(name)? {
            Some(run) => {
                self.spawn_headless(&self.agent(name)?, run, text)?;
                Ok("started".into())
            }
            None => {
                self.store.send("human", name, text, &agent.group_name)?;
                Ok("queued".into())
            }
        }
    }

    /// Whether the next run starts the agent's session, continues it, or
    /// continues a copy of an adopted one.
    fn session(&self, agent: &Agent) -> Session {
        if !harness::of(agent).picks_session_id() {
            return if agent.session_id.is_empty() {
                Session::New
            } else {
                Session::Resume
            };
        }
        match self.resume_mode(&agent.name).as_deref() {
            Some("fork") => Session::Fork,
            Some(_) => Session::Resume,
            None if self.session_logged(&agent.name) => Session::Resume,
            None => Session::New,
        }
    }

    /// Whether a run of this agent has started its Claude Code session.
    fn session_logged(&self, name: &str) -> bool {
        let runs = self.agent_dir(name).join("runs");
        std::fs::read_dir(runs)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.path().extension().is_some_and(|x| x == "jsonl")
                    && std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains("\"session_id\""))
            })
    }

    /// What a harness needs to start `agent`: drovr's MCP config and the
    /// system prompt, written fresh so they reflect the group as it is now.
    fn run_for<'a>(&'a self, agent: &'a Agent, session: Session) -> Result<Run<'a>> {
        Ok(Run {
            cfg: &self.cfg,
            agent,
            cli: self.cli.clone(),
            home: home(),
            mcp_config: self.mcp_config(&agent.name)?,
            system_prompt: self.system_prompt_file(agent)?,
            settings_file: self.agent_dir(&agent.name).join("herdr-settings.json"),
            session,
        })
    }

    fn spawn_headless(&self, agent: &Agent, run: i64, text: &str) -> Result<()> {
        use std::os::unix::process::CommandExt;
        let dir = self.agent_dir(&agent.name).join("runs");
        let prompt_file = dir.join(format!("{run}.prompt"));
        std::fs::write(&prompt_file, text)?;
        let harness = harness::of(agent);
        // After a checkpoint, the run starts a fresh session.
        let fresh = self.take_fresh(agent)?;
        let agent = fresh.as_ref().unwrap_or(agent);
        let session = if fresh.is_some() {
            Session::New
        } else {
            self.session(agent)
        };
        let args = harness.headless_args(&self.run_for(agent, session)?)?;
        self.record(
            &agent.name,
            "run_start",
            json!({
                "run": run,
                "harness": harness.name(),
                "model": harness::model_of(harness, agent, &self.cfg),
                "session": format!("{session:?}").to_lowercase(),
                "prompt": truncate(text, 4000),
            }),
        );

        let f = |ext: &str| quote(&dir.join(format!("{run}.{ext}")).display().to_string());
        let script = format!(
            "\"$0\" \"$@\" < {} > {} 2> {}; echo $? > {}; exec {} hook finished {}",
            f("prompt"),
            f("jsonl"),
            f("err"),
            f("exit"),
            quote(&self.cli.display().to_string()),
            quote(&agent.name)
        );
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg(harness.command(&self.cfg))
            .args(&args)
            .current_dir(&agent.cwd)
            .env("DROVR_AGENT", &agent.name)
            .env("DROVR_HOME", home())
            .envs(crate::datadog::env(&self.cfg))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", harness.command(&self.cfg)))?;
        self.store.set_pid(&agent.name, child.id() as i64)?;
        // Reap the wrapper so it doesn't linger as a zombie while we're alive.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// Called by the run wrapper when a headless run exits.
    pub fn finished(&self, name: &str) -> Result<()> {
        let agent = self.agent(name)?;
        let dir = self.agent_dir(name).join("runs");
        let exit: Option<i32> = std::fs::read_to_string(dir.join(format!("{}.exit", agent.runs)))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // No exit code means the run was killed before its wrapper finished:
        // a reboot, a crash, or the process being killed from outside.
        let interrupted = exit.is_none();
        let mut is_error = exit != Some(0);
        let mut end = None;
        let log = std::fs::read_to_string(dir.join(format!("{}.jsonl", agent.runs))).ok();
        if let Some(log) = &log {
            let harness = harness::of(&agent);
            let mut session = None;
            for v in log
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            {
                match harness.parse(&v) {
                    Event::Session(id) if session.is_none() => session = Some(id),
                    Event::End(e) => end = Some(e),
                    _ => {}
                }
            }
            // A forked session gets a new ID, and Codex names its session
            // when the first run starts: keep following it.
            let fork = self.resume_mode(name).as_deref() == Some("fork");
            if fork || !harness.picks_session_id() {
                if let Some(id) = session.filter(|id| !id.is_empty() && *id != agent.session_id) {
                    self.store.set_agent(name, "session_id", &id)?;
                    if fork {
                        std::fs::write(self.agent_dir(name).join("resumed"), "same")?;
                    }
                }
            }
            if let Some(end) = &end {
                is_error = end["is_error"].as_bool().unwrap_or(is_error);
            }
        }
        let total = self.total_cost(&agent);
        self.store.set_cost(name, total)?;
        self.store.set_pid(name, 0)?;
        let next = match agent.run_state.as_str() {
            "stopped" => "stopped",
            _ if interrupted => "interrupted",
            _ if is_error => "error",
            _ if agent.status.starts_with("done:") => "done",
            _ => "idle",
        };
        self.store.set_agent(name, "run_state", next)?;
        let end = end.unwrap_or_default();
        self.record(
            name,
            "run_end",
            json!({
                "run": agent.runs,
                "state": next,
                "exit": exit,
                "cost_usd": (total - agent.cost_usd).max(0.0),
                "duration_ms": end["duration_ms"],
                "turns": end["num_turns"],
                "tokens": end["tokens"],
                "models": end["models"],
            }),
        );
        // A stage agent may be asked for a checkpoint, or continued fresh.
        if (next == "idle" || next == "done")
            && self.after_stage_run(&agent, &end, log.as_deref().unwrap_or(""))?
        {
            return Ok(());
        }
        if next == "idle" || next == "done" {
            self.maybe_wake(name)?;
        }
        if next == "interrupted" && self.cfg.defaults.auto_resume {
            self.record(name, "wake", json!({ "reason": "interrupted" }));
            self.resume(name, RESUME_TEXT)?;
        }
        Ok(())
    }

    /// Re-prompt an idle agent when messages or answers are waiting for it.
    pub fn maybe_wake(&self, name: &str) -> Result<()> {
        if !self.cfg.defaults.auto_wake {
            return Ok(());
        }
        let Some(agent) = self.store.agent(name)? else {
            return Ok(());
        };
        if !matches!(agent.run_state.as_str(), "idle" | "done") {
            return Ok(());
        }
        let unread = self.store.unread(name, &agent.group_name)?;
        let answers = self.store.undelivered_answers(name)?;
        if unread.is_empty() && answers.is_empty() {
            return Ok(());
        }
        if agent.backend == "headless" && self.recent_runs(name, 600) >= WAKE_LIMIT {
            self.store.set_agent(
                name,
                "status",
                "auto-wake paused: too many runs in 10 minutes; prompt it to continue",
            )?;
            self.record(name, "wake_paused", json!({ "runs_in_10_min": WAKE_LIMIT }));
            return Ok(());
        }
        // Deliver everything inline and mark it read, so an agent that ignores
        // it isn't woken again for the same messages.
        let mut text = String::from("[drovr] New activity while you were idle:\n");
        for m in &unread {
            text += &format!(
                "- Message from {} (to {}): {}\n",
                self.cfg.display_name(&m.sender),
                self.cfg.display_name(&m.recipient),
                m.body
            );
        }
        if let Some(last) = unread.last() {
            self.store.mark_read(name, last.id)?;
        }
        for item in &answers {
            text += &format!("- {}\n", answer_text(item, &self.cfg.your_name()));
            self.store.mark_delivered(item.id)?;
        }
        text += "Continue your task with this in mind.";
        self.record(
            name,
            "wake",
            json!({ "reason": "activity", "messages": unread.len(), "answers": answers.len() }),
        );
        self.prompt(name, &text)?;
        Ok(())
    }

    fn recent_runs(&self, name: &str, secs: u64) -> usize {
        let cutoff = std::time::SystemTime::now() - Duration::from_secs(secs);
        std::fs::read_dir(self.agent_dir(name).join("runs"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "prompt"))
            .filter(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t > cutoff)
            })
            .count()
    }

    pub fn stop(&self, name: &str) -> Result<()> {
        let agent = self.agent(name)?;
        if agent.backend == "herdr" {
            if agent.run_state == "working" {
                let _ = self.herdr_end_run(&agent, "stopped");
            }
            self.herdr_close_pane(&agent);
        } else if run_alive(agent.pid, &agent.name) {
            // After a reboot the pid may belong to an unrelated process group.
            unsafe { libc::killpg(agent.pid as i32, libc::SIGTERM) };
        }
        self.store.set_agent(name, "run_state", "stopped")?;
        self.store.set_pid(name, 0)?;
        Ok(())
    }

    /// Deliver your queued message `id` now instead of when the run ends:
    /// stop the run if one is going, then start one with it, along with
    /// anything else that was waiting up to it.
    pub fn send_now(&self, name: &str, id: i64) -> Result<String> {
        let agent = self.agent(name)?;
        let unread: Vec<_> = self
            .store
            .unread(name, &agent.group_name)?
            .into_iter()
            .filter(|m| m.id <= id)
            .collect();
        if !unread.iter().any(|m| m.id == id) {
            bail!("{name} has already read that message");
        }
        let running = agent.backend == "headless" && run_alive(agent.pid, name);
        if running {
            self.stop(name)?;
            wait_for_group(agent.pid);
            self.store.set_cost(name, self.total_cost(&agent))?;
        }
        let mut text = String::new();
        if running {
            text += &format!(
                "[drovr] {} stopped your run to send this now, so it may have ended mid-step. \
                 Check where you were before carrying on.\n\n",
                self.cfg.your_name()
            );
        }
        let (yours, others): (Vec<_>, Vec<_>) = unread
            .iter()
            .partition(|m| m.sender == "human" && m.recipient == name);
        text += &yours
            .iter()
            .map(|m| m.body.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !others.is_empty() {
            text += "\n\n[drovr] Also waiting for you:\n";
            for m in others {
                text += &format!(
                    "- Message from {} (to {}): {}\n",
                    self.cfg.display_name(&m.sender),
                    self.cfg.display_name(&m.recipient),
                    m.body
                );
            }
        }
        self.store.mark_read(name, id)?;
        self.record(
            name,
            "send_now",
            json!({ "message": id, "stopped_run": running, "by": "you" }),
        );
        self.resume(name, &text)
    }

    // -- what you do -----------------------------------------------------------
    // The app and the CLI call these, so the record shows what you did.

    /// You prompt an agent.
    pub fn prompt_from_you(&self, name: &str, text: &str) -> Result<String> {
        // Before the run it starts, so the record reads in order. A busy
        // agent gets it queued.
        let busy = self.agent(name)?.run_state == "working";
        self.record(
            name,
            "you_prompt",
            json!({ "text": truncate(text, 4000), "queued": busy }),
        );
        self.prompt(name, text)
    }

    /// You stop an agent.
    pub fn stop_by_you(&self, name: &str) -> Result<()> {
        self.stop(name)?;
        self.record(name, "stopped", json!({ "by": "you" }));
        Ok(())
    }

    /// You resume an agent.
    pub fn resume_by_you(&self, name: &str, text: &str) -> Result<String> {
        self.record(
            name,
            "resumed",
            json!({ "by": "you", "text": truncate(text, 4000) }),
        );
        self.resume(name, text)
    }

    /// You take back a queued message.
    pub fn unqueue_by_you(&self, name: &str, id: i64) -> Result<bool> {
        let took = self.store.unqueue(name, id)?;
        if took {
            self.record(name, "unqueued", json!({ "message": id, "by": "you" }));
        }
        Ok(took)
    }

    /// You post on the message bus: to an agent, `@group` (with `group`),
    /// or `*` for everyone.
    pub fn send_from_you(&self, to: &str, group: &str, body: &str) -> Result<()> {
        self.store.send("human", to, body, group)?;
        let targets: Vec<String> = match to {
            "@group" => self
                .store
                .agents(Some(group))?
                .into_iter()
                .map(|a| a.name)
                .collect(),
            "*" => self
                .store
                .agents(None)?
                .into_iter()
                .map(|a| a.name)
                .collect(),
            name => vec![name.to_string()],
        };
        let event = json!({ "from": "you", "to": to, "body": truncate(body, 4000) });
        match to {
            "@group" => self.record_group(group, "message", event),
            "*" => {}
            name => self.record(name, "message", event),
        }
        for t in targets {
            self.maybe_wake(&t)?;
        }
        Ok(())
    }

    /// Bring a stopped or errored agent back by prompting it again.
    pub fn resume(&self, name: &str, text: &str) -> Result<String> {
        let agent = self.agent(name)?;
        if matches!(
            agent.run_state.as_str(),
            "stopped" | "error" | "interrupted"
        ) {
            self.store.set_agent(name, "run_state", "idle")?;
        }
        if agent.backend == "herdr" && agent.pane.is_empty()
            || agent.backend == "herdr" && agent.run_state == "stopped"
        {
            self.start_herdr(&agent)?;
        }
        let text = match (text.trim().is_empty(), agent.run_state.as_str()) {
            (false, _) => text,
            (true, "interrupted") => RESUME_TEXT,
            (true, _) => "[drovr] You were stopped. Pick up where you left off.",
        };
        self.prompt(name, text)
    }

    /// Stop and forget an agent. Its worktree goes too when it has nothing
    /// uncommitted; its `drovr/<name>` branch always stays. Returns a note
    /// about anything left behind.
    pub fn remove(&self, name: &str) -> Result<String> {
        // The name becomes a path below; `..` would delete far more than the agent.
        if !valid_name(name) {
            bail!("no agent named {name}");
        }
        let agent = self.store.agent(name)?;
        self.record(name, "agent_removed", json!({}));
        let _ = self.stop(name);
        self.store.remove_agent(name)?;
        let _ = std::fs::remove_dir_all(home().join("agents").join(name));
        let worktree = home().join("worktrees").join(name);
        match agent {
            Some(a) if Path::new(&a.cwd) == worktree => Ok(remove_worktree(&worktree, name)),
            _ => Ok(String::new()),
        }
    }

    /// Notice runs that died without reporting back (crash, reboot).
    pub fn refresh(&self) -> Result<()> {
        // drovr 0.9.0 and earlier added Claude Code's running session total,
        // so costs were counted many times over. Recount them once.
        let marker = home().join("costs-recounted");
        if !marker.exists() {
            for a in self.store.agents(None)? {
                self.store.set_cost(&a.name, self.total_cost(&a))?;
            }
            let _ = std::fs::write(&marker, "");
        }
        for agent in self.store.agents(None)? {
            if agent.backend == "headless"
                && agent.run_state == "working"
                && agent.pid > 0
                && !run_alive(agent.pid, &agent.name)
            {
                // The wrapper may have exited normally; let finished() decide.
                self.finished(&agent.name)?;
            }
        }
        Ok(())
    }

    /// Put an agent away. It keeps its output and session, and comes back if
    /// it's woken.
    pub fn archive(&self, name: &str) -> Result<()> {
        if self.agent(name)?.run_state == "working" {
            bail!("{name} is working; stop it or let it finish first");
        }
        self.store.archive_agent(name)
    }

    pub fn unarchive(&self, name: &str) -> Result<()> {
        self.store.unarchive_agent(name)
    }

    /// Archive every agent that's finished: done, stopped or failed, with
    /// nothing waiting for you. Returns how many.
    pub fn archive_finished(&self) -> Result<usize> {
        let archived = self.store.archived()?;
        let mut n = 0;
        for a in self.store.agents(None)? {
            let finished = matches!(
                self.display_state(&a).as_str(),
                "done" | "stopped" | "error"
            );
            if finished && !archived.contains(&a.name) {
                self.store.archive_agent(&a.name)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// State as shown to the human: `blocked` wins when the agent is waiting on them.
    pub fn display_state(&self, agent: &Agent) -> String {
        if self.store.pending_count(&agent.name).unwrap_or(0) > 0 {
            "blocked".into()
        } else {
            agent.run_state.clone()
        }
    }

    // -- groups and orchestration ---------------------------------------------

    /// Create a group; start its orchestrator when `spec.lead` is set.
    pub fn create_group(&self, spec: NewGroup) -> Result<Option<Agent>> {
        let name = &normalize_name(&spec.name);
        if !valid_name(name) {
            bail!("group names follow the same rules as agent names: up to 40 lowercase letters, digits, - and _, starting with a letter (got {name:?})");
        }
        if self.store.group(name)?.is_some() {
            bail!("a group named {name} already exists");
        }
        let plans_dir = spec.plans_dir.trim();
        if !plans_dir.is_empty() {
            crate::reports::check_folder(plans_dir)?;
        }
        let cwd = if spec.cwd.is_empty() {
            self.cfg.default_cwd()
        } else {
            expand_tilde(&spec.cwd)
        };
        self.store.add_group(
            name,
            &spec.goal,
            &spec.ticket,
            &cwd,
            plans_dir,
            self.cfg.workflow.enabled,
        )?;
        if !spec.lead {
            return Ok(None);
        }
        self.add_lead(name, &spec.goal, &spec.backend, &spec.harness, &spec.model)
            .map(Some)
    }

    /// Give a Jira ticket to an existing group: to `target` if named,
    /// otherwise to the group's orchestrator. The agent gets it as a prompt,
    /// or as a message it reads next if it's busy.
    pub fn hand_off_to_group(
        &self,
        group: &str,
        ticket: &str,
        task: &str,
        target: &str,
    ) -> Result<Agent> {
        self.store
            .group(group)?
            .ok_or_else(|| anyhow!("there's no group named {group}"))?;
        let members = self.store.agents(Some(group))?;
        let agent = if target.is_empty() {
            members
                .into_iter()
                .find(|a| a.role == "orchestrator")
                .ok_or_else(|| {
                    anyhow!("group {group} has no orchestrator; choose which agent gets the ticket")
                })?
        } else {
            members
                .into_iter()
                .find(|a| a.name == target)
                .ok_or_else(|| anyhow!("{target} isn't in group {group}"))?
        };
        // Track every ticket an agent works on, e.g. "SRE-3230, SRE-3253".
        let mut tickets: Vec<&str> = agent
            .ticket
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        if !ticket.is_empty() && !tickets.contains(&ticket) {
            tickets.push(ticket);
            self.store
                .set_agent(&agent.name, "ticket", &tickets.join(", "))?;
        }
        let how = if agent.role == "orchestrator" {
            "As the group's lead, plan this with the agents already in the group: hand parts to them with send_message, and spawn workers only if nobody fits."
        } else {
            "Coordinate with your peers in the group where this touches their work."
        };
        self.prompt(
            &agent.name,
            &format!("[drovr] New Jira ticket for group `{group}`.\n{how}\n\n{task}"),
        )?;
        self.agent(&agent.name)
    }

    /// Start an orchestrator for an existing group.
    pub fn add_lead(
        &self,
        group: &str,
        goal: &str,
        backend: &str,
        harness: &str,
        model: &str,
    ) -> Result<Agent> {
        self.start_agent(self.lead_spec(group, goal, backend, harness, model)?)
    }

    /// The orchestrator `add_lead` starts. An empty `model` is the default.
    fn lead_spec(
        &self,
        group: &str,
        goal: &str,
        backend: &str,
        harness: &str,
        model: &str,
    ) -> Result<NewAgent> {
        let g = self
            .store
            .group(group)?
            .ok_or_else(|| anyhow!("there's no group named {group}"))?;
        let goal = if goal.trim().is_empty() {
            g.goal.clone()
        } else {
            goal.to_string()
        };
        let task = format!(
            "{goal}

Plan the work, spawn workers with spawn_worker (or coordinate the agents already in the group), and bring me decisions and commands through the drovr inbox."
        );
        Ok(NewAgent {
            name: format!("{group}-lead"),
            group: group.into(),
            role: "orchestrator".into(),
            backend: backend.into(),
            harness: harness.into(),
            cwd: g.cwd,
            ticket: g.ticket,
            task,
            model: model.trim().into(),
            ..NewAgent::default()
        })
    }

    // -- the record ------------------------------------------------------------

    /// Add an event to the record of each ticket `agent` works on: its own
    /// tickets, else its group's. An agent with no ticket has no record, and
    /// nothing is recorded with the ticket workflow off (`workflow.enabled`).
    /// Never fails: a record that can't be written mustn't stop the work.
    pub fn record(&self, agent: &str, kind: &str, data: Value) {
        if let Ok(Some(a)) = self.store.agent(agent) {
            let who = json!({ "agent": a.name, "role": a.role });
            self.record_tickets(&a.group_name, &a.ticket, who, kind, data);
        }
    }

    /// Add an event to the record of a group's ticket.
    pub fn record_group(&self, group: &str, kind: &str, data: Value) {
        self.record_tickets(group, "", json!({}), kind, data);
    }

    fn record_tickets(&self, group: &str, tickets: &str, who: Value, kind: &str, data: Value) {
        if !self.cfg.workflow.enabled {
            return;
        }
        let g = match group {
            "" => None,
            g => self.store.group(g).ok().flatten(),
        };
        let mut keys = ledger::tickets_in(tickets);
        if keys.is_empty() {
            keys = g
                .as_ref()
                .map(|g| ledger::tickets_in(&g.ticket))
                .unwrap_or_default();
        }
        let folder = g
            .as_ref()
            .map(|g| g.plans_folder().to_string())
            .unwrap_or_default();
        let mut event = json!({ "group": group });
        if let Some(e) = event.as_object_mut() {
            for part in [who, data] {
                if let Value::Object(fields) = part {
                    e.extend(fields);
                }
            }
            // The event's kind wins over any `kind` in its data.
            e.insert("kind".into(), json!(kind));
        }
        for key in keys {
            if let Err(e) = ledger::append(&self.cfg, &folder, &key, &event) {
                eprintln!("drovr: couldn't add to {key}'s record: {e:#}");
            }
        }
    }

    /// Add an event to a ticket's record from outside any agent (the
    /// watcher), in every folder its agents and groups record to. A ticket
    /// nobody has picked up has no record.
    pub fn record_ticket(&self, key: &str, kind: &str, data: Value) {
        if !self.cfg.workflow.enabled {
            return;
        }
        let key = key.to_uppercase();
        let groups = self.store.groups().unwrap_or_default();
        let folder_of = |group: &str| {
            groups
                .iter()
                .find(|g| g.name == group)
                .map(|g| g.plans_folder().to_string())
                .unwrap_or_default()
        };
        let mut folders = std::collections::BTreeSet::new();
        for g in &groups {
            if ledger::tickets_in(&g.ticket).contains(&key) {
                folders.insert(g.plans_folder().to_string());
            }
        }
        for a in self.store.agents(None).unwrap_or_default() {
            if ledger::tickets_in(&a.ticket).contains(&key) {
                folders.insert(folder_of(&a.group_name));
            }
        }
        let mut event = json!({ "source": "watcher" });
        if let (Some(e), Value::Object(fields)) = (event.as_object_mut(), data) {
            e.extend(fields);
            e.insert("kind".into(), json!(kind));
        }
        for folder in folders {
            if let Err(e) = ledger::append(&self.cfg, &folder, &key, &event) {
                eprintln!("drovr: couldn't add to {key}'s record: {e:#}");
            }
        }
    }

    /// Record an Inbox item: once when it's asked, again when it's resolved.
    pub fn record_item(&self, id: i64) {
        let Ok(Some(i)) = self.store.item(id) else {
            return;
        };
        let mut data = json!({ "item": i.id, "type": i.kind, "title": truncate(&i.title, 1000) });
        let more = if i.status == "pending" {
            json!({ "body": truncate(&i.body, 4000), "options": i.options, "payload": clip(&i.payload, 4000) })
        } else {
            json!({
                "status": i.status,
                "answer": truncate(&i.answer, 4000),
                "waited_s": (crate::store::now() - i.ts).round(),
            })
        };
        if let (Some(d), Value::Object(m)) = (data.as_object_mut(), more) {
            d.extend(m);
        }
        let kind = if i.status == "pending" {
            "asked"
        } else {
            "answered"
        };
        // The watcher's items have no agent; they belong to their ticket.
        match (self.store.agent(&i.agent), i.payload["ticket"].as_str()) {
            (Ok(None), Some(ticket)) => self.record_ticket(ticket, kind, data),
            _ => self.record(&i.agent, kind, data),
        }
    }

    /// The plans folder a group's reports and prompts live in.
    pub fn plans_folder(&self, group: &str) -> Result<String> {
        if group.is_empty() {
            return Ok(String::new());
        }
        Ok(self
            .store
            .group(group)?
            .map(|g| g.plans_folder().to_string())
            .unwrap_or_else(|| group.to_string()))
    }

    /// An orchestrator asks for a worker. Returns the worker, or the inbox item
    /// id when the human has to approve the spawn first.
    /// `stage` and `model` are the ticket workflow's: a stage's agent, named
    /// after its stage by default and run on the stage's model.
    pub fn spawn_worker(
        &self,
        from: &str,
        name: &str,
        task: &str,
        cwd: &str,
        stage: &str,
        model: &str,
    ) -> Result<Value> {
        let lead = self.agent(from)?;
        if lead.role != "orchestrator" || lead.group_name.is_empty() {
            bail!("only a group's orchestrator can spawn workers");
        }
        if !stage.is_empty() && !self.cfg.stages.order.iter().any(|s| s == stage) {
            bail!(
                "there's no stage {stage:?}; the stages are {}",
                self.cfg.stages.order.join(", ")
            );
        }
        let name = if name.trim().is_empty() { stage } else { name };
        if name.trim().is_empty() {
            bail!("name is required");
        }
        let group = lead.group_name.clone();
        // Stage agents that finished (stopped) don't count against the limit.
        let workers = self
            .store
            .agents(Some(&group))?
            .into_iter()
            .filter(|a| a.role == "worker" && !(!a.stage.is_empty() && a.run_state == "stopped"))
            .count();
        if workers >= self.cfg.orchestrator.max_workers {
            bail!(
                "group {group} already has {workers} workers (max {})",
                self.cfg.orchestrator.max_workers
            );
        }
        let name = worker_name(&group, name);
        let spec = NewAgent {
            name: name.clone(),
            group,
            role: "worker".into(),
            backend: lead.backend.clone(),
            harness: lead.harness.clone(),
            cwd: if cwd.is_empty() {
                lead.cwd.clone()
            } else {
                cwd.into()
            },
            ticket: lead.ticket.clone(),
            task: task.into(),
            stage: stage.into(),
            model: worker_model(&lead, self.cfg.orchestrator.auto_spawn, stage, model),
            ..NewAgent::default()
        };
        if self.cfg.orchestrator.auto_spawn {
            let a = self.start_agent(spec)?;
            return Ok(json!({ "spawned": a.name, "cwd": a.cwd }));
        }
        // What it runs on if you leave the Inbox's model box empty.
        let mut payload = serde_json::to_value(&spec)?;
        payload["default_model"] = match agent_model(&self.cfg, &spec.model, &spec.stage) {
            m if m.is_empty() => crate::harness::named(&spec.harness)
                .map(|h| h.model(&self.cfg).to_string())
                .unwrap_or_default(),
            m => m,
        }
        .into();
        let id = self.store.add_item(
            from,
            "spawn",
            &format!("Spawn worker {name}?"),
            task,
            &[],
            &payload,
        )?;
        self.record_item(id);
        Ok(json!({ "pending_approval": id, "name": name }))
    }

    // -- inbox actions --------------------------------------------------------

    pub fn answer(&self, id: i64, answer: &str) -> Result<()> {
        let item = self.pending_item(id)?;
        match item.kind.as_str() {
            // "approve", maybe with "model=…" for the worker.
            "spawn" => {
                let mut spec: NewAgent = serde_json::from_value(item.payload.clone())?;
                if let Some(model) = picked_model(answer) {
                    spec.model = model;
                }
                // Other workers may have started since the lead asked.
                let workers = self
                    .store
                    .agents(Some(&spec.group))?
                    .iter()
                    .filter(|a| a.role == "worker")
                    .count();
                let max = self.cfg.orchestrator.max_workers;
                if workers >= max {
                    bail!("group {} already has {workers} workers (max {max}); remove one first, or raise orchestrator.max_workers", spec.group);
                }
                // Start first: if it fails, the request stays in the Inbox.
                self.start_agent(spec)?;
                self.store.resolve_item(id, "answered", "approved")?;
                self.store.send(
                    "drovr",
                    &item.agent,
                    &format!(
                        "{} approved spawning {}.",
                        capitalize(&self.cfg.your_name()),
                        item.title
                    ),
                    "",
                )?;
            }
            "command" => {
                self.store.resolve_item(id, "answered", answer)?;
            }
            // A stage's handoff: "Approve" (maybe "model=…" for the next
            // stage), or "Send back" with notes.
            "handoff" => {
                if let Some(notes) = answer.strip_prefix("Send back") {
                    let notes = notes.trim_start_matches([':', ' ']);
                    self.store.resolve_item(id, "rejected", notes)?;
                    self.record_item(id);
                    return self.send_back_handoff(&item, notes);
                }
                self.store.resolve_item(id, "answered", answer)?;
                self.record_item(id);
                return self.approve_handoff(&item, answer);
            }
            // The watcher asked: "Send to <agent>" passes what changed on.
            "watch" => {
                self.store.resolve_item(id, "answered", answer)?;
                if let (Some(agent), Some(text), true) = (
                    item.payload["agent"].as_str(),
                    item.payload["text"].as_str(),
                    answer.starts_with("Send to "),
                ) {
                    self.record(
                        agent,
                        "wake",
                        json!({ "reason": item.payload["trigger"], "ticket": item.payload["ticket"], "by": "you" }),
                    );
                    self.prompt(agent, text)?;
                }
            }
            // A Jira update you approved, as you edited it: the comment's
            // text, or the status to move to. Posting first: if it fails,
            // the draft stays in the Inbox.
            "jira" => {
                let p = &item.payload;
                let key = p["ticket"].as_str().unwrap_or("");
                let sources = crate::sources::Sources::new(&self.cfg);
                match p["action"].as_str() {
                    Some("comment") => sources.jira_comment(key, answer)?,
                    _ => {
                        sources.jira_transition(key, answer, p["fields"].as_str().unwrap_or(""))?
                    }
                };
                let done = match p["action"].as_str() {
                    Some("comment") => format!("Posted:\n{answer}"),
                    _ => format!("Moved to {answer}"),
                };
                self.store.resolve_item(id, "answered", &done)?;
            }
            // A page you approved: saved as a Confluence draft. If saving
            // fails, or the page changed since your diff, it stays here.
            "confluence" => {
                let done = crate::confluence::save(self, &item)?;
                self.store.resolve_item(id, "answered", &done)?;
            }
            _ => {
                self.store.resolve_item(id, "answered", answer)?;
            }
        }
        self.record_item(id);
        self.maybe_wake(&item.agent)
    }

    /// An inbox item that still waits on you.
    fn pending_item(&self, id: i64) -> Result<Item> {
        let item = self
            .store
            .item(id)?
            .ok_or_else(|| anyhow!("no inbox item {id}"))?;
        if item.status != "pending" {
            bail!("inbox item {id} was already resolved ({})", item.status);
        }
        Ok(item)
    }

    pub fn reject(&self, id: i64, reason: &str) -> Result<()> {
        let item = self.pending_item(id)?;
        self.store.resolve_item(id, "rejected", reason)?;
        self.record_item(id);
        if item.kind == "handoff" {
            return self.send_back_handoff(&item, reason);
        }
        self.maybe_wake(&item.agent)
    }

    /// You ran a proposed command yourself (copied it to a terminal). Tells the
    /// agent it ran, with whatever output or note you pasted.
    pub fn mark_ran(&self, id: i64, note: &str) -> Result<()> {
        let item = self.pending_item(id)?;
        if item.kind != "command" {
            return Err(anyhow!("item {id} is a {}, not a command", item.kind));
        }
        let command = item.payload["command"].as_str().unwrap_or_default();
        let note = note.trim();
        let result = format!(
            "{} ran `{command}` themselves, outside drovr.\n{}",
            capitalize(&self.cfg.your_name()),
            if note.is_empty() {
                "Output was not captured; ask them if you need it, or check the result yourself."
                    .to_string()
            } else {
                format!(
                    "Their note, or the output from their terminal:\n{}",
                    truncate(note, 20_000)
                )
            }
        );
        self.store.resolve_item(id, "answered", &result)?;
        self.record_item(id);
        self.maybe_wake(&item.agent)
    }

    pub fn dismiss(&self, id: i64) -> Result<()> {
        self.store.resolve_item(id, "dismissed", "")?;
        self.record_item(id);
        Ok(())
    }

    /// Run an approved command. `where_` is "local" (output goes back to the
    /// agent) or "herdr" (opens a pane so the human can watch it).
    pub fn run_command(
        &self,
        id: i64,
        where_: &str,
        command_override: Option<&str>,
    ) -> Result<String> {
        // A command runs once: not again after it was answered, and not twice
        // at the same time (say, Run and Run in herdr pane clicked together).
        let item = self.pending_item(id)?;
        if item.kind != "command" {
            bail!("item {id} is a {}, not a command", item.kind);
        }
        let _running = RunningCommand::claim(id)?;
        let command = command_override
            .filter(|c| !c.trim().is_empty())
            .map(String::from)
            .or_else(|| item.payload["command"].as_str().map(String::from))
            .ok_or_else(|| anyhow!("item {id} has no command"))?;
        let cwd = item.payload["cwd"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| self.cfg.default_cwd());
        let result = if where_ == "herdr" {
            let pane = self.herdr_split(&cwd)?;
            self.herdr_run_script(&pane, &format!("cd {} && {command}", quote(&cwd)))?;
            format!("{} ran `{command}` in herdr pane {pane}. Output was not captured; ask them if you need it.", capitalize(&self.cfg.your_name()))
        } else {
            let out = shell::sh(
                &command,
                Some(Path::new(&cwd)),
                None,
                Duration::from_secs(self.cfg.commands.timeout_seconds),
            )?;
            format!(
                "{} approved and ran `{command}` in {cwd}.\nexit: {}{}\nstdout:\n{}\nstderr:\n{}",
                capitalize(&self.cfg.your_name()),
                out.code,
                if out.timed_out { " (timed out)" } else { "" },
                truncate(&out.stdout, 20_000),
                truncate(&out.stderr, 5_000)
            )
        };
        self.store.resolve_item(id, "answered", &result)?;
        self.record_item(id);
        self.maybe_wake(&item.agent)?;
        Ok(result)
    }

    // -- output ---------------------------------------------------------------

    pub fn output(&self, name: &str, max_lines: usize) -> Result<Vec<Line>> {
        let agent = self.agent(name)?;
        if agent.backend == "herdr" {
            return self.herdr_read(&agent, max_lines);
        }
        let dir = self.agent_dir(name).join("runs");
        let harness = harness::of(&agent);
        // Each run: its number, prompt, rendered log and stderr.
        type Run = (i64, Option<String>, Arc<Vec<Rendered>>, Option<String>);
        let runs: Vec<Run> = (1..=agent.runs)
            .map(|run| {
                let prompt = std::fs::read_to_string(dir.join(format!("{run}.prompt"))).ok();
                let log = rendered_log(&dir.join(format!("{run}.jsonl")), harness);
                let err = std::fs::read_to_string(dir.join(format!("{run}.err")))
                    .ok()
                    .filter(|e| !e.trim().is_empty())
                    .map(|e| truncate(e.trim(), 4000));
                (run, prompt, log, err)
            })
            .collect();
        // Only the last `max_lines` are copied out, but every run is walked:
        // what a run cost depends on the runs before it.
        let total: usize = runs
            .iter()
            .map(|(_, p, log, e)| 1 + p.is_some() as usize + log.len() + e.is_some() as usize)
            .sum();
        let mut skip = total.saturating_sub(max_lines);
        let mut lines = Vec::with_capacity(total - skip);
        let mut costs = Costs::default();
        let mut push = |line: &dyn Fn() -> Line| {
            if skip > 0 {
                skip -= 1;
            } else {
                lines.push(line());
            }
        };
        for (run, prompt, log, err) in &runs {
            push(&|| Line {
                kind: "run".into(),
                text: format!("run {run}"),
            });
            if let Some(p) = prompt {
                push(&|| Line {
                    kind: "prompt".into(),
                    text: p.clone(),
                });
            }
            for item in log.iter() {
                match item {
                    Rendered::Line(l) => push(&|| l.clone()),
                    Rendered::Result(v) => {
                        let line = result_line(v, harness, &mut costs);
                        push(&|| line.clone());
                    }
                }
            }
            if let Some(e) = err {
                push(&|| Line {
                    kind: "error".into(),
                    text: e.clone(),
                });
            }
        }
        Ok(lines)
    }

    /// Command that opens the agent's session interactively in a terminal.
    pub fn attach_command(&self, name: &str) -> Result<String> {
        let agent = self.agent(name)?;
        let run = self.run_for(&agent, Session::Resume)?;
        Ok(format!(
            "cd {} && DROVR_AGENT={} {}",
            quote(&agent.cwd),
            quote(name),
            harness::of(&agent).attach_command(&run)?
        ))
    }

    /// Open a headless agent's session interactively in a new herdr pane.
    pub fn open_in_herdr(&self, name: &str) -> Result<String> {
        let agent = self.agent(name)?;
        if let Some(pane) = self.focus_live_pane(&agent)? {
            return Ok(pane);
        }
        if agent.run_state == "working" {
            bail!("{name} is mid-run; stop it first so two sessions don't write to the same conversation");
        }
        let cmd = self.attach_command(name)?;
        let pane = self.herdr_split(&agent.cwd)?;
        self.herdr_run_script(&pane, &cmd)?;
        Ok(pane)
    }

    /// Show the pane a herdr agent is still running in, because resuming its
    /// session elsewhere would put two processes on one conversation.
    fn focus_live_pane(&self, agent: &Agent) -> Result<Option<String>> {
        if agent.backend != "herdr" || agent.pane.is_empty() {
            return Ok(None);
        }
        let target = [("target", agent.pane.as_str())];
        let get = self.herdr_fill(&self.cfg.herdr.get, &target);
        if !shell::sh(&get, None, None, Duration::from_secs(10))?.ok() {
            return Ok(None);
        }
        let focus = self.herdr_fill(&self.cfg.herdr.focus, &target);
        shell::sh(&focus, None, None, Duration::from_secs(10))?.into_stdout("herdr agent focus")?;
        Ok(Some(agent.pane.clone()))
    }

    // -- herdr ----------------------------------------------------------------

    fn herdr_fill(&self, template: &str, vars: &[(&str, &str)]) -> String {
        let mut all = vec![("herdr", self.cfg.herdr.command.as_str())];
        all.extend_from_slice(vars);
        shell::fill(template, &all, &["herdr", "lines", "args"])
    }

    fn herdr_split(&self, cwd: &str) -> Result<String> {
        self.herdr_split_from(cwd, std::env::var("HERDR_PANE_ID").ok().as_deref())
    }

    /// `--current` reads `HERDR_PANE_ID`, which only herdr's own panes have, and
    /// the app usually isn't started from one. There it means the focused pane.
    fn herdr_split_from(&self, cwd: &str, inherited: Option<&str>) -> Result<String> {
        let cmd = self.herdr_fill(&self.cfg.herdr.split, &[("cwd", cwd)]);
        self.herdr_new_pane(cmd, inherited)
    }

    /// The pane an agent starts in.
    fn herdr_agent_pane(&self, agent: &Agent) -> Result<String> {
        let cmd = self.herdr_fill(
            &self.cfg.herdr.agent_pane,
            &[("cwd", &agent.cwd), ("name", &agent.name)],
        );
        self.herdr_new_pane(cmd, std::env::var("HERDR_PANE_ID").ok().as_deref())
    }

    fn herdr_new_pane(&self, mut cmd: String, inherited: Option<&str>) -> Result<String> {
        if cmd.contains("--current") {
            let pane = match inherited.filter(|pane| !pane.is_empty()) {
                Some(pane) => pane.to_string(),
                None => self.herdr_focused_pane()?,
            };
            cmd = format!("export HERDR_PANE_ID={}; {cmd}", quote(&pane));
        }
        let out = shell::sh(&cmd, None, None, Duration::from_secs(30))?
            .into_stdout("opening a herdr pane")?;
        split_pane_id(&out).ok_or_else(|| anyhow!("couldn't find a pane id in herdr output: {out}"))
    }

    fn herdr_focused_pane(&self) -> Result<String> {
        let cmd = self.herdr_fill(&self.cfg.herdr.list, &[]);
        let out = shell::sh(&cmd, None, None, Duration::from_secs(10))?
            .into_stdout("herdr pane list")
            .map_err(|e| anyhow!("couldn't find a herdr pane to split. Is herdr running? {e}"))?;
        focused_pane(&out)
            .ok_or_else(|| anyhow!("herdr has no focused pane to split. Open herdr and try again."))
    }

    fn start_herdr(&self, agent: &Agent) -> Result<()> {
        let session = match self.resume_mode(&agent.name).as_deref() {
            Some("fork") => Session::Fork,
            Some(_) => Session::Resume,
            None if agent.runs > 0 => Session::Resume,
            None => Session::New,
        };
        let first = (agent.runs == 0 && !agent.task.is_empty()).then_some(agent.task.as_str());
        self.herdr_open_session(agent, session, first)
    }

    fn herdr_open_session(
        &self,
        agent: &Agent,
        session: Session,
        first: Option<&str>,
    ) -> Result<()> {
        let args = harness::of(agent).herdr_args(&self.run_for(agent, session)?)?;
        let pane = self.herdr_agent_pane(agent)?;
        self.store.set_agent(&agent.name, "pane", &pane)?;
        let args: Vec<String> = args.iter().map(|a| quote(a)).collect();
        let start = self.herdr_fill(
            &self.cfg.herdr.start,
            &[
                ("name", &herdr_name(&agent.name)),
                ("pane", &pane),
                ("args", &args.join(" ")),
            ],
        );
        self.herdr_start_when_ready(&agent.name, &start, &pane, HERDR_READY_WAIT)?;
        if let Some(text) = first {
            let name = format!("{session:?}").to_lowercase();
            self.herdr_begin_run(&agent.name, &name, text)?;
            self.herdr_prompt(&self.agent(&agent.name)?, text)?;
        }
        Ok(())
    }

    fn herdr_close_pane(&self, agent: &Agent) {
        if !agent.pane.is_empty() {
            let cmd = self.herdr_fill(&self.cfg.herdr.stop, &[("pane", &agent.pane)]);
            let _ = shell::sh(&cmd, None, None, Duration::from_secs(15));
        }
    }

    fn herdr_load_run(&self, name: &str) -> HerdrRun {
        std::fs::read_to_string(self.agent_dir(name).join("herdr-run.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn herdr_save_run(&self, name: &str, run: &HerdrRun) -> Result<()> {
        std::fs::write(
            self.agent_dir(name).join("herdr-run.json"),
            serde_json::to_string(run)?,
        )?;
        Ok(())
    }

    /// Add what the session has cost since this was last called to the
    /// agent's cost and the current run's.
    fn herdr_track_cost(&self, name: &str) -> Result<(HerdrRun, Option<harness::Live>)> {
        let agent = self.agent(name)?;
        let mut run = self.herdr_load_run(name);
        let live = std::fs::read_to_string(self.agent_dir(name).join("status-line.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| harness::of(&agent).live_status(&v));
        if let Some(live) = &live {
            // A lower total means a new count (a new or forked session).
            let added = if live.session == run.session && live.total_cost_usd >= run.total {
                live.total_cost_usd - run.total
            } else {
                live.total_cost_usd
            };
            run.session = live.session.clone();
            run.total = live.total_cost_usd;
            run.run_cost += added;
            self.store.set_cost(name, agent.cost_usd + added)?;
            self.herdr_save_run(name, &run)?;
        }
        Ok((run, live))
    }

    /// A run starts, unless one already is going.
    fn herdr_begin_run(&self, name: &str, session: &str, prompt: &str) -> Result<()> {
        let (mut state, _) = self.herdr_track_cost(name)?;
        let Some(run) = self.store.claim_run(name)? else {
            return Ok(());
        };
        state.run_cost = 0.0;
        state.started = crate::store::now();
        self.herdr_save_run(name, &state)?;
        let agent = self.agent(name)?;
        let harness = harness::of(&agent);
        self.record(
            name,
            "run_start",
            json!({
                "run": run,
                "harness": harness.name(),
                "model": harness::model_of(harness, &agent, &self.cfg),
                "session": session,
                "prompt": truncate(prompt, 4000),
            }),
        );
        Ok(())
    }

    /// A run ended in `state`. Returns what `after_stage_run` reads: an
    /// interactive session gives no per-run tokens, turns or exit code.
    fn herdr_end_run(&self, agent: &Agent, state: &str) -> Result<Value> {
        let (mut run, live) = self.herdr_track_cost(&agent.name)?;
        let model = live.as_ref().map(|l| l.model.clone());
        self.record(
            &agent.name,
            "run_end",
            json!({
                "run": self.agent(&agent.name)?.runs,
                "state": state,
                "exit": null,
                "cost_usd": run.run_cost,
                "duration_ms": ((crate::store::now() - run.started) * 1000.0) as i64,
                "turns": null,
                "tokens": null,
                "models": model.filter(|m| !m.is_empty()).map(|m| vec![m]),
            }),
        );
        run.run_cost = 0.0;
        self.herdr_save_run(&agent.name, &run)?;
        Ok(json!({
            "total_cost_usd": live.as_ref().map(|l| l.total_cost_usd),
            "context_tokens": live.as_ref().map(|l| l.context_tokens),
        }))
    }

    /// Run `start`, which herdr refuses until the new pane's shell is at its
    /// prompt. A pane that never gets there is empty, so it's closed.
    fn herdr_start_when_ready(
        &self,
        name: &str,
        start: &str,
        pane: &str,
        wait: Duration,
    ) -> Result<()> {
        let began = std::time::Instant::now();
        loop {
            let out = shell::sh(start, None, None, Duration::from_secs(90))?;
            if out.ok() || !out.stderr.contains("agent_pane_busy") {
                out.into_stdout("herdr agent start")?;
                return Ok(());
            }
            if began.elapsed() >= wait {
                let stop = self.herdr_fill(&self.cfg.herdr.stop, &[("pane", pane)]);
                let _ = shell::sh(&stop, None, None, Duration::from_secs(10));
                self.store.set_agent(name, "pane", "")?;
                let reason = out.into_stdout("herdr agent start").unwrap_err();
                bail!("herdr's new pane wasn't ready, so drovr closed it: {reason}");
            }
            std::thread::sleep(HERDR_READY_POLL);
        }
    }

    /// herdr types prompts into the terminal, and macOS rejects any input
    /// line longer than its canonical limit (about 1 KB). Long or multi-line
    /// prompts go in a file and the agent is told to read it.
    fn herdr_prompt(&self, agent: &Agent, text: &str) -> Result<()> {
        if agent.pane.is_empty() {
            bail!("{} has no herdr pane", agent.name);
        }
        let text = if text.len() > HERDR_INLINE_MAX || text.contains('\n') {
            let dir = self.agent_dir(&agent.name).join("prompts");
            std::fs::create_dir_all(&dir)?;
            let file = dir.join(format!("{}.md", (crate::store::now() * 1000.0) as i64));
            std::fs::write(&file, text)?;
            format!("[drovr] Read {} and act on it.", file.display())
        } else {
            text.to_string()
        };
        let cmd = self.herdr_fill(
            &self.cfg.herdr.prompt,
            &[("target", &agent.pane), ("text", &text)],
        );
        shell::sh(&cmd, None, None, Duration::from_secs(30))?.into_stdout("herdr agent prompt")?;
        Ok(())
    }

    /// Run a shell command in a herdr pane via a script file, so the line
    /// typed into the pane stays short.
    fn herdr_run_script(&self, pane: &str, script: &str) -> Result<()> {
        let dir = home().join("scripts");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{}.sh", (crate::store::now() * 1000.0) as i64));
        std::fs::write(&file, script)?;
        let line = format!("sh {}", quote(&file.display().to_string()));
        let run = self.herdr_fill(&self.cfg.herdr.run, &[("pane", pane), ("cmd", &line)]);
        shell::sh(&run, None, None, Duration::from_secs(30))?.into_stdout("herdr pane run")?;
        Ok(())
    }

    fn herdr_read(&self, agent: &Agent, max_lines: usize) -> Result<Vec<Line>> {
        if agent.pane.is_empty() {
            return Ok(vec![]);
        }
        let n = max_lines.to_string();
        let cmd = self.herdr_fill(
            &self.cfg.herdr.read,
            &[("target", &agent.pane), ("lines", &n)],
        );
        let out = shell::sh(&cmd, None, None, Duration::from_secs(15))?
            .into_stdout("herdr agent read")?;
        let text = serde_json::from_str::<Value>(&out)
            .ok()
            .and_then(|v| find_key(&v, &["text", "content", "output"]))
            .unwrap_or(out);
        Ok(text
            .lines()
            .map(|l| Line {
                kind: "raw".into(),
                text: l.to_string(),
            })
            .collect())
    }

    /// Map herdr's view of agent states (working/blocked/done/idle) onto ours.
    pub fn refresh_herdr(&self) -> Result<()> {
        for agent in self.store.agents(None)? {
            if agent.backend != "herdr" || agent.pane.is_empty() || agent.run_state == "stopped" {
                continue;
            }
            let cmd = self.herdr_fill(&self.cfg.herdr.get, &[("target", &agent.pane)]);
            let Ok(out) = shell::sh(&cmd, None, None, Duration::from_secs(10)) else {
                continue;
            };
            if !out.ok() {
                // The pane, or the agent in it, is gone.
                if agent.run_state == "working" {
                    self.herdr_end_run(&agent, "stopped")?;
                }
                self.store.set_agent(&agent.name, "run_state", "stopped")?;
                continue;
            }
            let (run, _) = self.herdr_track_cost(&agent.name)?;
            let state = match herdr_status(&out.stdout).as_deref() {
                Some("working") | Some("blocked") => "working",
                Some("idle") | Some("done") => {
                    if agent.status.starts_with("done:") {
                        "done"
                    } else {
                        "idle"
                    }
                }
                _ => continue,
            };
            if state == agent.run_state {
                continue;
            }
            // You typed into the pane.
            if state == "working" {
                self.herdr_begin_run(&agent.name, "resume", "")?;
                continue;
            }
            let ended = agent.run_state == "working";
            if ended && crate::store::now() - run.started < HERDR_RUN_SETTLE {
                continue;
            }
            self.store.set_agent(&agent.name, "run_state", state)?;
            if ended {
                let end = self.herdr_end_run(&agent, state)?;
                if self.after_stage_run(&self.agent(&agent.name)?, &end, "")? {
                    continue;
                }
            }
            self.maybe_wake(&agent.name)?;
        }
        Ok(())
    }
}

/// The new pane's id from `herdr pane split` output.
fn split_pane_id(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout).unwrap_or(Value::String(stdout.trim().to_string()));
    // Each key searches the whole output: herdr's top-level "id" names the
    // request, so it mustn't win over a nested "pane_id".
    ["pane_id", "pane", "id"]
        .iter()
        .find_map(|key| find_key(&v, &[key]))
        .or_else(|| v.as_str().filter(|s| !s.is_empty()).map(String::from))
}

/// The focused pane's id from `herdr pane list` output:
/// `{"result":{"panes":[{"focused":true,"pane_id":"w1:p1", …}]}}`.
fn focused_pane(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout).ok()?;
    v["result"]["panes"]
        .as_array()?
        .iter()
        .find(|pane| pane["focused"] == true)?["pane_id"]
        .as_str()
        .map(String::from)
}

/// The agent's status from `herdr agent get` output:
/// `{"result":{"agent":{"agent_status":"done", "pane_id":"w7:p1", …}}}`.
fn herdr_status(stdout: &str) -> Option<String> {
    let v: Value = serde_json::from_str(stdout).ok()?;
    v["result"]["agent"]["agent_status"]
        .as_str()
        .map(String::from)
        .or_else(|| find_key(&v, &["agent_status", "state", "status"]))
}

/// herdr agent names: `[a-z][a-z0-9_-]{0,31}`.
fn herdr_name(name: &str) -> String {
    name.chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches(['-', '_'])
        .to_string()
}

pub fn answer_text(item: &Item, you: &str) -> String {
    let you = capitalize(you);
    match (item.kind.as_str(), item.status.as_str()) {
        ("command", "rejected") => format!(
            "{you} rejected command #{} ({}): {}",
            item.id, item.title, item.answer
        ),
        ("command", _) => format!("Command #{} result:\n{}", item.id, item.answer),
        ("confluence", "rejected") => format!(
            "{you} sent back your Confluence page #{} ({}): {}",
            item.id, item.title, item.answer
        ),
        ("confluence" | "jira", _) => format!("#{} ({}): {}", item.id, item.title, item.answer),
        (_, "rejected") => format!(
            "{you} declined to answer #{} ({}): {}",
            item.id, item.title, item.answer
        ),
        _ => format!(
            "{you} answered #{} ({}): {}",
            item.id, item.title, item.answer
        ),
    }
}

/// "the human" → "The human" at the start of a sentence; names are unchanged.
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Turn one stream-json event into readable lines.
/// The line for the end of a run (a harness's `Event::End`): how it ended,
/// how long it took, and what it cost (which depends on the runs before it,
/// through `costs`) or the tokens it used, as far as the harness says.
fn result_line(v: &Value, harness: &dyn Harness, costs: &mut Costs) -> Line {
    let mut text = String::from(if v["is_error"] == true {
        "failed"
    } else {
        "finished"
    });
    if let Some(ms) = v["duration_ms"].as_f64() {
        text += &format!(" in {:.0}s", ms / 1000.0);
    }
    if let Some(cost) = harness.run_cost(v, costs) {
        text += &format!(" · ${cost:.3}");
    }
    if let (Some(input), Some(output)) = (v["input_tokens"].as_u64(), v["output_tokens"].as_u64()) {
        text += &format!(" · {input} tokens in, {output} out");
    }
    if let Some(turns) = v["num_turns"].as_i64() {
        text += &format!(" · {turns} turns");
    }
    Line {
        kind: "final".into(),
        text,
    }
}

/// One event of a run log, ready to show. The end of a run keeps what its
/// line needs, since its cost is worked out when the output is put together.
#[derive(Clone)]
enum Rendered {
    Line(Line),
    Result(Value),
}

/// A run log parsed up to `parsed_to`. Logs only grow while a run writes
/// them, so the next read starts there.
struct RunLog {
    /// The file's inode: a removed agent's logs can be replaced by a new
    /// agent's with the same name.
    ino: u64,
    parsed_to: u64,
    items: Arc<Vec<Rendered>>,
}

/// Run logs already parsed in this process. The app asks for an agent's
/// output every 1.5 s; without this it re-read and re-parsed every log of
/// every run each time.
fn run_logs() -> &'static Mutex<HashMap<PathBuf, RunLog>> {
    static LOGS: OnceLock<Mutex<HashMap<PathBuf, RunLog>>> = OnceLock::new();
    LOGS.get_or_init(Default::default)
}

/// A run log's events, parsing only what was added since the last call.
fn rendered_log(path: &Path, harness: &dyn Harness) -> Arc<Vec<Rendered>> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return Arc::default();
    };
    let mut logs = run_logs().lock().unwrap_or_else(|e| e.into_inner());
    if logs.len() > 4096 {
        logs.clear();
    }
    let fresh = || RunLog {
        ino: meta.ino(),
        parsed_to: 0,
        items: Arc::default(),
    };
    let log = logs.entry(path.to_path_buf()).or_insert_with(fresh);
    if log.ino != meta.ino() || meta.len() < log.parsed_to {
        *log = fresh();
    }
    if meta.len() > log.parsed_to {
        let mut buf = Vec::new();
        if let Ok(mut f) = std::fs::File::open(path) {
            if f.seek(SeekFrom::Start(log.parsed_to)).is_ok() {
                let _ = f.take(meta.len() - log.parsed_to).read_to_end(&mut buf);
            }
        }
        // Whole lines only: the run may be mid-way through writing the last
        // one. A last line without a newline counts once it's complete JSON.
        let mut end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        if end < buf.len() && serde_json::from_slice::<Value>(&buf[end..]).is_ok() {
            end = buf.len();
        }
        let items = Arc::make_mut(&mut log.items);
        for line in String::from_utf8_lossy(&buf[..end]).lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match harness.parse(&v) {
                Event::Lines(lines) => items.extend(lines.into_iter().map(Rendered::Line)),
                Event::End(end) => items.push(Rendered::Result(end)),
                Event::Session(_) | Event::Skip => {}
            }
        }
        log.parsed_to += end as u64;
    }
    log.items.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_runs_on_its_model_then_its_stages_then_the_default() {
        let mut cfg = Config::default();
        cfg.stages
            .models
            .insert("research".into(), "claude-haiku-5-5".into());
        for (picked, stage, want) in [
            ("", "", ""),
            ("claude-opus-5-5", "", "claude-opus-5-5"),
            ("", "research", "claude-haiku-5-5"),
            ("claude-opus-5-5", "research", "claude-opus-5-5"),
            ("  claude-opus-5-5 ", "research", "claude-opus-5-5"),
            ("", "build", ""),
            ("   ", "research", "claude-haiku-5-5"),
        ] {
            assert_eq!(
                agent_model(&cfg, picked, stage),
                want,
                "{picked:?} {stage:?}"
            );
        }
    }

    #[test]
    fn workers_you_dont_approve_run_on_the_leads_model() {
        let lead = |model: &str| Agent {
            model: model.into(),
            ..Agent::default()
        };
        for (lead_model, auto, stage, asked, want) in [
            // Started without you: the lead's model, unless it names one.
            ("claude-opus-5-5", true, "", "", "claude-opus-5-5"),
            (
                "claude-opus-5-5",
                true,
                "",
                "claude-sonnet-5-5",
                "claude-sonnet-5-5",
            ),
            ("", true, "", "", ""),
            // A stage follows the handoff you approved: its own model or the default.
            ("claude-opus-5-5", true, "research", "", ""),
            (
                "claude-opus-5-5",
                true,
                "research",
                "claude-sonnet-5-5",
                "claude-sonnet-5-5",
            ),
            // You approve it in the Inbox, where you can pick one.
            ("claude-opus-5-5", false, "", "", ""),
            (
                "claude-opus-5-5",
                false,
                "",
                "claude-sonnet-5-5",
                "claude-sonnet-5-5",
            ),
        ] {
            assert_eq!(
                worker_model(&lead(lead_model), auto, stage, asked),
                want,
                "{lead_model:?} auto={auto} {stage:?} {asked:?}"
            );
        }
    }

    #[test]
    fn approving_a_worker_offers_its_default_model() {
        let dir = std::env::temp_dir().join(format!("drovr-spawn-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.orchestrator.auto_spawn = false;
        cfg.defaults.auto_wake = false;
        cfg.defaults.model = "claude-sonnet-5-5".into();
        cfg.stages
            .models
            .insert("research".into(), "claude-haiku-5-5".into());
        let engine = Engine::with(
            Store::open(&dir.join("t.db")).unwrap(),
            cfg,
            PathBuf::from("drovr"),
        );
        engine
            .store
            .add_group("g", "", "", "/tmp", "", true)
            .unwrap();
        engine
            .store
            .add_agent(&Agent {
                name: "g-lead".into(),
                group_name: "g".into(),
                role: "orchestrator".into(),
                model: "claude-opus-5-5".into(),
                run_state: "working".into(),
                ..Agent::default()
            })
            .unwrap();
        let offered = |name: &str, stage: &str| {
            let r = engine
                .spawn_worker("g-lead", name, "do it", "", stage, "")
                .unwrap();
            let item = engine
                .store
                .item(r["pending_approval"].as_i64().unwrap())
                .unwrap()
                .unwrap();
            (
                item.payload["model"].as_str().unwrap().to_string(),
                item.payload["default_model"].as_str().unwrap().to_string(),
            )
        };
        assert_eq!(offered("api", ""), ("".into(), "claude-sonnet-5-5".into()));
        assert_eq!(
            offered("", "research"),
            ("".into(), "claude-haiku-5-5".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_model_you_type_when_approving() {
        for (answer, want) in [
            ("approve", None),
            ("Approve", None),
            ("approve model=claude-opus-5-5", Some("claude-opus-5-5")),
            ("Approve model= claude-opus-5-5 ", Some("claude-opus-5-5")),
            ("Approve model=  ", None),
        ] {
            assert_eq!(picked_model(answer).as_deref(), want, "{answer:?}");
        }
    }

    #[test]
    fn a_lead_runs_on_the_model_you_pick() {
        let dir = std::env::temp_dir().join(format!("drovr-lead-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = Engine::with(
            Store::open(&dir.join("t.db")).unwrap(),
            Config::default(),
            PathBuf::from("drovr"),
        );
        engine
            .create_group(NewGroup {
                name: "pay".into(),
                goal: "ship refunds".into(),
                cwd: "/tmp".into(),
                ..NewGroup::default()
            })
            .unwrap();
        let spec = engine
            .lead_spec("pay", "", "headless", "", "claude-opus-5-5")
            .unwrap();
        assert_eq!(
            (spec.name.as_str(), spec.role.as_str(), spec.model.as_str()),
            ("pay-lead", "orchestrator", "claude-opus-5-5")
        );
        assert!(spec.task.starts_with("ship refunds"));
        // Empty keeps the configured default.
        assert_eq!(engine.lead_spec("pay", "", "", "", "").unwrap().model, "");
        assert!(engine.lead_spec("nope", "", "", "", "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_and_slugs() {
        assert!(valid_name("pay-123-lead"));
        assert!(!valid_name("Pay"));
        assert!(!valid_name("1abc"));
        assert_eq!(
            slug("API: add /refunds endpoint"),
            "api-add-refunds-endpoint"
        );
        assert_eq!(slug("123 fix"), "a-123-fix");
        assert_eq!(normalize_name(" Fix   Login "), "fix-login");
        assert_eq!(normalize_name("Pay-API lead"), "pay-api-lead");
        // Not guessed at: these still fail validation.
        assert!(!valid_name(&normalize_name("1st try")));
        assert!(!valid_name(&normalize_name("fix/login")));
        assert_eq!(worker_name("pay", "API tests"), "pay-api-tests");
        assert_eq!(worker_name("pay", "pay-api"), "pay-api");
        let long = worker_name("payments-refund-rework", "integration tests for refunds");
        assert!(valid_name(&long), "{long}");
    }

    #[test]
    fn remove_refuses_paths() {
        let dir = std::env::temp_dir().join(format!("drovr-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = Engine::with(
            Store::open(&dir.join("t.db")).unwrap(),
            Config::default(),
            PathBuf::from("drovr"),
        );
        for name in ["..", "../..", "a/../..", ""] {
            assert!(engine.remove(name).is_err(), "{name:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_what_happens_on_a_ticket() {
        let dir = std::env::temp_dir().join(format!("drovr-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = dir.join("plans").display().to_string();
        // Nothing here may start a run.
        cfg.defaults.auto_wake = false;
        // Off, nothing is recorded.
        let off = Engine::with(
            Store::open(&dir.join("off.db")).unwrap(),
            cfg.clone(),
            PathBuf::from("drovr"),
        );
        off.store
            .add_agent(&Agent {
                name: "api".into(),
                ticket: "SRE-9".into(),
                ..Agent::default()
            })
            .unwrap();
        off.record("api", "status", json!({}));
        off.send_from_you("api", "", "hi").unwrap();
        assert!(ledger::list(&off.cfg).is_empty());
        cfg.workflow.enabled = true;
        let engine = Engine::with(
            Store::open(&dir.join("t.db")).unwrap(),
            cfg,
            PathBuf::from("drovr"),
        );
        let st = &engine.store;
        st.add_group("wif", "", "SRE-3229", "/tmp", "sre-3229", false)
            .unwrap();
        let agent = |name: &str, group: &str, ticket: &str| Agent {
            name: name.into(),
            group_name: group.into(),
            role: if group.is_empty() { "solo" } else { "worker" }.into(),
            ticket: ticket.into(),
            ..Agent::default()
        };
        st.add_agent(&agent("api", "wif", "")).unwrap();
        st.add_agent(&agent("solo", "", "SRE-1, SRE-2")).unwrap();
        st.add_agent(&agent("scratch", "", "")).unwrap();

        // A group's agent writes to the group's ticket, in the group's folder.
        engine.record("api", "status", json!({ "status": "planning" }));
        let id = st
            .add_item(
                "api",
                "decision",
                "Per SA or all at once?",
                "",
                &["per SA".into(), "all".into()],
                &json!({}),
            )
            .unwrap();
        engine.record_item(id);
        engine.answer(id, "per SA").unwrap();
        engine.send_from_you("@group", "wif", "heads up").unwrap();
        // A solo agent writes to each of its tickets; one with none, nowhere.
        engine.record("solo", "status", json!({}));
        engine.record("scratch", "status", json!({}));

        let kinds = |ticket: &str| -> Vec<String> {
            ledger::timeline(&engine.cfg, ticket)
                .iter()
                .map(|e| {
                    format!(
                        "{}:{}",
                        e["kind"].as_str().unwrap(),
                        e["agent"].as_str().unwrap_or("")
                    )
                })
                .collect()
        };
        assert_eq!(
            kinds("SRE-3229"),
            ["status:api", "asked:api", "answered:api", "message:"]
        );
        let t = ledger::timeline(&engine.cfg, "SRE-3229");
        assert_eq!(
            (
                t[2]["status"].as_str(),
                t[2]["answer"].as_str(),
                t[2]["type"].as_str()
            ),
            (Some("answered"), Some("per SA"), Some("decision"))
        );
        assert_eq!(
            (
                t[3]["from"].as_str(),
                t[3]["to"].as_str(),
                t[3]["group"].as_str()
            ),
            (Some("you"), Some("@group"), Some("wif"))
        );
        assert!(dir.join("plans/sre-3229/SRE-3229/ledger.jsonl").is_file());
        assert_eq!(kinds("SRE-1"), ["status:solo"]);
        assert_eq!(kinds("SRE-2"), ["status:solo"]);
        assert_eq!(ledger::list(&engine.cfg).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_groups_made_with_the_workflow_on_work_in_stages() {
        let dir = std::env::temp_dir().join(format!("drovr-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.defaults.auto_wake = false;
        let db = dir.join("t.db");
        let group = |name: &str| NewGroup {
            name: name.into(),
            cwd: "/tmp".into(),
            ..NewGroup::default()
        };
        // A group from before the workflow was turned on…
        Engine::with(
            Store::open(&db).unwrap(),
            cfg.clone(),
            PathBuf::from("drovr"),
        )
        .create_group(group("old"))
        .unwrap();
        cfg.workflow.enabled = true;
        let engine = Engine::with(Store::open(&db).unwrap(), cfg, PathBuf::from("drovr"));
        // …and one made with it on.
        engine.create_group(group("new")).unwrap();
        let lead = |g: &str| Agent {
            name: format!("{g}-lead"),
            group_name: g.into(),
            role: "orchestrator".into(),
            ..Agent::default()
        };
        assert!(!engine.store.group("old").unwrap().unwrap().workflow);
        assert!(!engine.staged(&lead("old")));
        assert!(engine.staged(&lead("new")));
        assert!(!engine.staged(&Agent::default()));
        // Turned off, no group works in stages.
        let mut off = engine.cfg.clone();
        off.workflow.enabled = false;
        let engine = Engine::with(Store::open(&db).unwrap(), off, PathBuf::from("drovr"));
        assert!(!engine.staged(&lead("new")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_agent_writes_and_takes_its_own_ticket() {
        let dir = std::env::temp_dir().join(format!("drovr-ticket-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.defaults.your_name = "Izz".into();
        let engine = Engine::with(
            Store::open(&dir.join("t.db")).unwrap(),
            cfg,
            PathBuf::from("drovr"),
        );
        // What the agent is told first: the generic way, then with your skill.
        let first = engine.ticket_first("a bug, assigned to me, in the current sprint");
        assert!(first.contains("acceptance criteria"));
        assert!(first.contains("Izz asks for: a bug, assigned to me, in the current sprint."));
        assert!(first.contains("`set_ticket`") && first.ends_with("## The task\n\n"));
        let mut engine = engine;
        engine.cfg.jira.ticket_skill = "~/.claude/skills/ticket-writing/SKILL.md".into();
        assert!(engine
            .ticket_first("")
            .contains("ticket-writing skill at `~/.claude/skills/ticket-writing/SKILL.md`"));
        // set_ticket: the agent's ticket, and its group's when it leads one without.
        engine
            .store
            .add_group("rel", "", "", "/tmp", "", false)
            .unwrap();
        let lead = Agent {
            name: "rel-lead".into(),
            group_name: "rel".into(),
            role: "orchestrator".into(),
            ..Agent::default()
        };
        engine.store.add_agent(&lead).unwrap();
        assert!(engine.set_ticket(&lead, "no key here").is_err());
        assert!(engine.set_ticket(&lead, "SRE-1 SRE-2").is_err());
        assert_eq!(
            engine.set_ticket(&lead, "sre-3401").unwrap(),
            "your ticket is SRE-3401"
        );
        assert_eq!(
            engine.store.agent("rel-lead").unwrap().unwrap().ticket,
            "SRE-3401"
        );
        assert_eq!(
            engine.store.group("rel").unwrap().unwrap().ticket,
            "SRE-3401"
        );
        // A group that has a ticket keeps it.
        engine.set_ticket(&lead, "SRE-3402").unwrap();
        assert_eq!(
            engine.store.group("rel").unwrap().unwrap().ticket,
            "SRE-3401"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worktrees_come_back_and_go_cleanly() {
        let dir = std::env::temp_dir().join(format!("drovr-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |cwd: &Path, cmd: &str| {
            let o = shell::sh(cmd, Some(cwd), None, Duration::from_secs(30)).unwrap();
            assert!(o.ok(), "{cmd}: {}", o.stderr);
            o.stdout
        };
        sh(&repo, "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init");
        let root = dir.join("worktrees");
        let cwd = repo.display().to_string();

        let wt = make_worktree(&root, &cwd, "api").unwrap();
        assert_eq!(wt, root.join("api").display().to_string());
        // Starting again while the worktree is there reuses it.
        assert_eq!(make_worktree(&root, &cwd, "api").unwrap(), wt);
        // A commit on the branch, then a clean removal: the folder goes, the branch stays.
        sh(Path::new(&wt), "echo a > a.txt && git add a.txt && git -c user.name=t -c user.email=t@t commit -q -m a");
        assert!(remove_worktree(Path::new(&wt), "api").contains("removed"));
        assert!(!Path::new(&wt).exists());
        // The same name again picks up the old branch and its commit.
        let wt = make_worktree(&root, &cwd, "api").unwrap();
        assert!(Path::new(&wt).join("a.txt").exists());
        // Uncommitted work keeps the worktree.
        std::fs::write(Path::new(&wt).join("b.txt"), "b").unwrap();
        assert!(remove_worktree(Path::new(&wt), "api").contains("kept"));
        assert!(Path::new(&wt).join("b.txt").exists());
        // Outside a repo there's no worktree.
        let plain = dir.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let p = plain.display().to_string();
        assert_eq!(make_worktree(&root, &p, "web").unwrap(), p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_nested_pane_id() {
        let v: Value = serde_json::from_str(r#"{"result":{"pane":{"pane_id":"w1:p3"}}}"#).unwrap();
        assert_eq!(find_key(&v, &["pane_id"]).unwrap(), "w1:p3");
    }

    #[test]
    fn reads_herdr_agent_get() {
        // Real `herdr agent get w7:p1` output.
        let out = r#"{"id":"cli:agent:get","result":{"agent":{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"225e9ac0-9c9c-4f7c-927e-49054bff82a0"},"agent_status":"done","cwd":"/Users/x/develop","focused":false,"pane_id":"w7:p1","revision":32,"state_change_seq":1578,"tab_id":"w7:t1","terminal_id":"term_65e3465ece130d","terminal_title":"✳ pay-api coordinator session restart","workspace_id":"w7"},"type":"agent_info"}}"#;
        assert_eq!(herdr_status(out).as_deref(), Some("done"));
        assert_eq!(herdr_status("not json"), None);
    }

    /// An engine whose `herdr` is a stub that logs each call to the returned
    /// file, with `agent get` and `agent focus` exiting as asked.
    fn engine_with_logging_herdr(case: &str, get: i32, focus: i32) -> (Engine, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("drovr-herdr-open-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls");
        let stub = dir.join("herdr");
        std::fs::write(
            &stub,
            format!(
                r#"echo "$1 $2 $3" >> {log}
case "$1 $2" in
"agent get") exit {get} ;;
"agent focus") exit {focus} ;;
esac
"#,
                log = log.display()
            ),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.herdr.command = format!("sh {}", stub.display());
        let store = Store::open(&dir.join("t.db")).unwrap();
        (Engine::with(store, cfg, "drovr".into()), log)
    }

    fn add_herdr_test_agent(engine: &Engine, backend: &str, pane: &str, state: &str) -> Agent {
        engine
            .store
            .add_agent(&Agent {
                name: "a".into(),
                backend: backend.into(),
                cwd: "/tmp".into(),
                ..Agent::default()
            })
            .unwrap();
        engine.store.set_agent("a", "pane", pane).unwrap();
        engine.store.set_agent("a", "run_state", state).unwrap();
        engine.agent("a").unwrap()
    }

    #[test]
    fn opening_a_live_herdr_agent_focuses_its_pane() {
        for state in ["idle", "working"] {
            let (engine, log) = engine_with_logging_herdr(&format!("live-{state}"), 0, 0);
            add_herdr_test_agent(&engine, "herdr", "w1:p2", state);
            assert_eq!(engine.open_in_herdr("a").unwrap(), "w1:p2", "{state}");
            assert_eq!(
                std::fs::read_to_string(&log).unwrap(),
                "agent get w1:p2\nagent focus w1:p2\n",
                "{state}"
            );
        }
    }

    #[test]
    fn says_so_when_a_live_herdr_pane_wont_focus() {
        let (engine, log) = engine_with_logging_herdr("no-focus", 0, 1);
        add_herdr_test_agent(&engine, "herdr", "w1:p2", "idle");
        let err = engine.open_in_herdr("a").unwrap_err().to_string();
        assert!(err.contains("herdr agent focus"), "{err}");
        assert!(!std::fs::read_to_string(&log).unwrap().contains("split"));
    }

    #[test]
    fn only_a_live_herdr_pane_gets_focused() {
        for (case, backend, pane, get, want_calls) in [
            ("gone", "herdr", "w1:p2", 1, "agent get w1:p2\n"),
            ("no-pane", "herdr", "", 0, ""),
            ("headless", "headless", "w1:p2", 0, ""),
        ] {
            let (engine, log) = engine_with_logging_herdr(case, get, 0);
            let agent = add_herdr_test_agent(&engine, backend, pane, "idle");
            assert_eq!(engine.focus_live_pane(&agent).unwrap(), None, "{case}");
            assert_eq!(
                std::fs::read_to_string(&log).unwrap_or_default(),
                want_calls,
                "{case}"
            );
        }
    }

    #[test]
    fn opening_a_working_agent_without_a_live_pane_is_refused() {
        for (case, backend, get) in [("gone-busy", "herdr", 1), ("busy", "headless", 0)] {
            let (engine, log) = engine_with_logging_herdr(case, get, 0);
            add_herdr_test_agent(&engine, backend, "w1:p2", "working");
            let err = engine.open_in_herdr("a").unwrap_err().to_string();
            assert!(err.contains("mid-run"), "{case}: {err}");
            let calls = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(!calls.contains("split"), "{case}: {calls}");
        }
    }

    #[test]
    fn reads_the_pane_id_from_herdr_pane_split() {
        // Real `herdr pane split` output from herdr 0.9.1 first: its top-level
        // "id" names the request, not the pane.
        for (out, want) in [
            (
                r#"{"id":"cli:pane:split","result":{"pane":{"agent_status":"unknown","cwd":"/private/tmp","focused":false,"pane_id":"w1:p4","tab_id":"w1:t1","workspace_id":"w1"},"type":"pane_info"}}"#,
                Some("w1:p4"),
            ),
            // Real `herdr tab create` output.
            (
                r#"{"id":"cli:tab:create","result":{"root_pane":{"agent_status":"unknown","cwd":"/private/tmp","focused":false,"pane_id":"w1:pP","tab_id":"w1:tD","workspace_id":"w1"},"tab":{"focused":false,"label":"zz-shape","number":13,"pane_count":1,"tab_id":"w1:tD","workspace_id":"w1"},"type":"tab_created"}}"#,
                Some("w1:pP"),
            ),
            (r#"{"pane":"w2:p1"}"#, Some("w2:p1")),
            (r#"{"id":"w3:p1"}"#, Some("w3:p1")),
            ("w4:p1\n", Some("w4:p1")),
            ("", None),
        ] {
            assert_eq!(split_pane_id(out).as_deref(), want, "{out}");
        }
    }

    /// An engine whose `herdr` is a stub: `pane list` runs `list`, and
    /// `pane split` behaves like the real one, needing `HERDR_PANE_ID` for
    /// `--current` and naming the pane it split from in the new pane's id.
    fn engine_with_herdr_stub(case: &str, list: &str, split: &str) -> Engine {
        let dir =
            std::env::temp_dir().join(format!("drovr-herdr-split-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("herdr");
        std::fs::write(
            &stub,
            format!(
                r#"case "$2" in
list) {list} ;;
split)
  if [ "$3" = --current ]; then
    [ -n "$HERDR_PANE_ID" ] || {{ echo '--current requires HERDR_PANE_ID' >&2; exit 2; }}
    from="$HERDR_PANE_ID"
  else
    from="$3"
  fi
  printf '{{"id":"cli:pane:split","result":{{"pane":{{"pane_id":"beside-%s"}}}}}}' "$from"
  ;;
esac
"#
            ),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.herdr.command = format!("sh {}", stub.display());
        if !split.is_empty() {
            cfg.herdr.split = split.into();
        }
        Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into())
    }

    #[test]
    fn splits_the_focused_pane_when_drovr_isnt_inside_herdr() {
        // Real `herdr pane list` shape: one workspace's pane is in the background.
        let two_panes = r#"printf '%s' '{"id":"cli:pane:list","result":{"panes":[{"focused":false,"pane_id":"w2:p1","workspace_id":"w2"},{"focused":true,"pane_id":"w1:p3","workspace_id":"w1"}],"type":"pane_list"}}'"#;
        let list_fails = "echo 'list must not run' >&2; exit 1";
        for (case, list, split, inherited, want) in [
            ("outside", two_panes, "", None, "beside-w1:p3"),
            ("empty-env", two_panes, "", Some(""), "beside-w1:p3"),
            ("inside", list_fails, "", Some("w5:p2"), "beside-w5:p2"),
            (
                "custom",
                list_fails,
                "{herdr} pane split w6:p1 --cwd {cwd}",
                None,
                "beside-w6:p1",
            ),
        ] {
            let engine = engine_with_herdr_stub(case, list, split);
            let pane = engine
                .herdr_split_from("/tmp", inherited)
                .unwrap_or_else(|e| panic!("{case}: {e}"));
            assert_eq!(pane, want, "{case}");
        }
    }

    #[test]
    fn says_so_when_there_is_no_herdr_pane_to_split() {
        let no_server = r#"echo '{"error":{"code":"server_not_running"}}' >&2; exit 1"#;
        let unfocused =
            r#"printf '%s' '{"result":{"panes":[{"focused":false,"pane_id":"w2:p1"}]}}'"#;
        let no_panes = r#"printf '%s' '{"result":{"panes":[]}}'"#;
        for (case, list, want) in [
            ("no-server", no_server, "server_not_running"),
            ("unfocused", unfocused, "no focused pane"),
            ("no-panes", no_panes, "no focused pane"),
        ] {
            let err = engine_with_herdr_stub(case, list, "")
                .herdr_split_from("/tmp", None)
                .unwrap_err()
                .to_string();
            assert!(err.contains("herdr"), "{case}: {err}");
            assert!(err.contains(want), "{case}: {err}");
            assert!(!err.contains("requires HERDR_PANE_ID"), "{case}: {err}");
        }
    }

    /// An engine whose `herdr` logs each call and answers like the real one,
    /// with `agent start` failing with `code` for its first `fails` tries.
    fn engine_with_slow_shell(case: &str, code: &str, fails: u32) -> (Engine, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("drovr-herdr-ready-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls");
        let tries = dir.join("tries");
        let stub = dir.join("herdr");
        std::fs::write(
            &stub,
            format!(
                r#"echo "$*" >> {log}
case "$1 $2" in
"tab create") printf '%s' '{{"result":{{"root_pane":{{"pane_id":"w1:p7"}},"tab":{{"tab_id":"w1:t7"}}}}}}' ;;
"pane split") printf '%s' '{{"result":{{"pane":{{"pane_id":"w1:p8"}}}}}}' ;;
"agent start")
  echo x >> {tries}
  if [ "$(wc -l < {tries})" -le {fails} ]; then
    echo '{{"error":{{"code":"{code}"}}}}' >&2
    exit 1
  fi
  ;;
esac
"#,
                log = log.display(),
                tries = tries.display()
            ),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.herdr.command = format!("sh {}", stub.display());
        let store = Store::open(&dir.join("t.db")).unwrap();
        let engine = Engine::with(store, cfg, "drovr".into());
        engine
            .store
            .add_agent(&Agent {
                name: "a".into(),
                backend: "herdr".into(),
                cwd: "/tmp".into(),
                ..Agent::default()
            })
            .unwrap();
        engine.store.set_agent("a", "pane", "w1:p7").unwrap();
        (engine, log)
    }

    #[test]
    fn an_agent_starts_in_its_own_tab_and_commands_stay_beside_you() {
        let (engine, log) = engine_with_slow_shell("tab", "", 0);
        let agent = engine.agent("a").unwrap();
        assert_eq!(engine.herdr_agent_pane(&agent).unwrap(), "w1:p7");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "tab create --cwd /tmp --label a --no-focus\n"
        );
        assert!(engine.cfg.herdr.split.contains("pane split"));
    }

    #[test]
    fn starting_in_a_new_pane_waits_for_its_shell() {
        let busy = "agent_pane_busy";
        for (case, code, fails, wait_ms, started, starts, closed, pane) in [
            ("ready", busy, 0, 5000, true, 1, false, "w1:p7"),
            ("slow", busy, 2, 5000, true, 3, false, "w1:p7"),
            ("never", busy, 99, 300, false, 2, true, ""),
            (
                "other",
                "agent_not_ready",
                99,
                5000,
                false,
                1,
                false,
                "w1:p7",
            ),
        ] {
            let (engine, log) = engine_with_slow_shell(case, code, fails);
            let start = engine.herdr_fill(
                &engine.cfg.herdr.start,
                &[("name", "a"), ("pane", "w1:p7"), ("args", "")],
            );
            let got =
                engine.herdr_start_when_ready("a", &start, "w1:p7", Duration::from_millis(wait_ms));
            assert_eq!(got.is_ok(), started, "{case}: {got:?}");
            if let Err(e) = got {
                assert!(e.to_string().contains(code), "{case}: {e}");
            }
            let calls = std::fs::read_to_string(&log).unwrap();
            assert_eq!(
                calls.matches("agent start").count(),
                starts,
                "{case}: {calls}"
            );
            assert_eq!(
                calls.contains("pane close w1:p7"),
                closed,
                "{case}: {calls}"
            );
            assert_eq!(engine.agent("a").unwrap().pane, pane, "{case}");
        }
    }

    /// An engine, in a group with a ticket, with one herdr agent in pane
    /// `w1:p1`. `herdr` is a stub that logs its calls to the returned file,
    /// and `agent get` reports the status in its `state` file, or fails
    /// when there's none.
    fn engine_with_herdr_agent(
        case: &str,
        change: impl FnOnce(&mut Agent),
    ) -> (Engine, String, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("drovr-herdr-run-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls");
        let stub = dir.join("herdr");
        std::fs::write(
            &stub,
            format!(
                r#"echo "$*" >> {log}
case "$1 $2" in
"agent get")
  [ -f {state} ] || exit 1
  printf '{{"result":{{"agent":{{"agent_status":"%s"}}}}}}' "$(cat {state})"
  ;;
"tab create") printf '%s' '{{"result":{{"root_pane":{{"pane_id":"w1:p2"}},"tab":{{}}}}}}' ;;
esac
"#,
                log = log.display(),
                state = dir.join("state").display(),
            ),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.herdr.command = format!("sh {}", stub.display());
        cfg.workflow.enabled = true;
        cfg.defaults.auto_wake = false;
        cfg.reports.dir = dir.join("plans").display().to_string();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let engine = Engine::with(store, cfg, "drovr".into());
        engine
            .store
            .add_group("g", "", "SRE-1", "/tmp", "", true)
            .unwrap();
        let name = format!("run-{case}");
        let mut agent = Agent {
            name: name.clone(),
            group_name: "g".into(),
            backend: "herdr".into(),
            cwd: "/tmp".into(),
            ticket: "SRE-1".into(),
            session_id: "s1".into(),
            ..Agent::default()
        };
        change(&mut agent);
        engine.store.add_agent(&agent).unwrap();
        engine.store.set_agent(&name, "pane", "w1:p1").unwrap();
        engine.store.set_agent(&name, "run_state", "idle").unwrap();
        (engine, name, dir)
    }

    fn status_line(name: &str, session: &str, total: f64) {
        save_status_line(
            name,
            &json!({
                "session_id": session,
                "cost": { "total_cost_usd": total },
                "model": { "id": "claude-haiku-5-5" },
                "context_window": { "current_usage": { "input_tokens": 1000 } },
            })
            .to_string(),
        )
        .unwrap();
    }

    /// Past the moment when herdr may still say a prompted agent is idle.
    fn settle_run(engine: &Engine, name: &str) {
        let mut run = engine.herdr_load_run(name);
        run.started -= HERDR_RUN_SETTLE + 1.0;
        engine.herdr_save_run(name, &run).unwrap();
    }

    fn events(engine: &Engine, kind: &str) -> Vec<Value> {
        ledger::timeline(&engine.cfg, "SRE-1")
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .collect()
    }

    #[test]
    fn a_status_line_is_saved_only_for_a_real_agent_name() {
        assert!(save_status_line("../evil", "{}").is_err());
        save_status_line("saved-status", "{\"a\":1}").unwrap();
        let file = home().join("agents/saved-status/status-line.json");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn a_herdr_agents_cost_counts_each_session_once() {
        let (engine, name, _) = engine_with_herdr_agent("count", |_| {});
        for (session, total, cost) in [
            ("s1", 1.0, 1.0),
            ("s1", 1.5, 1.5),
            // The same figure again, as on every refresh.
            ("s1", 1.5, 1.5),
            // A resumed session keeps counting.
            ("s1", 1.75, 1.75),
            // A new session, then a lower total for it.
            ("s2", 0.25, 2.0),
            ("s2", 0.125, 2.125),
        ] {
            status_line(&name, session, total);
            engine.herdr_track_cost(&name).unwrap();
            let got = engine.agent(&name).unwrap().cost_usd;
            assert!((got - cost).abs() < 1e-9, "{session} {total}: {got}");
        }
    }

    #[test]
    fn a_herdr_run_is_recorded_with_its_cost_and_model() {
        let (engine, name, dir) = engine_with_herdr_agent("record", |_| {});
        status_line(&name, "s1", 1.0);
        engine.prompt(&name, "do it").unwrap();
        // A prompt while it's busy isn't another run.
        engine.prompt(&name, "and this").unwrap();
        status_line(&name, "s1", 1.5);
        std::fs::write(dir.join("state"), "idle").unwrap();
        settle_run(&engine, &name);
        engine.refresh_herdr().unwrap();

        let agent = engine.agent(&name).unwrap();
        assert_eq!(agent.run_state, "idle");
        assert_eq!(agent.runs, 1);
        assert!((agent.cost_usd - 1.5).abs() < 1e-9, "{}", agent.cost_usd);
        let starts = events(&engine, "run_start");
        assert_eq!(starts.len(), 1, "{starts:?}");
        assert_eq!(starts[0]["prompt"], "do it");
        let ends = events(&engine, "run_end");
        assert_eq!(ends.len(), 1, "{ends:?}");
        assert_eq!(ends[0]["run"], 1);
        assert_eq!(ends[0]["state"], "idle");
        assert_eq!(ends[0]["cost_usd"], 0.5);
        assert_eq!(ends[0]["models"], json!(["claude-haiku-5-5"]));
    }

    #[test]
    fn a_just_prompted_herdr_agent_isnt_taken_for_finished() {
        let (engine, name, dir) = engine_with_herdr_agent("settle", |_| {});
        engine.prompt(&name, "do it").unwrap();
        std::fs::write(dir.join("state"), "idle").unwrap();
        engine.refresh_herdr().unwrap();
        assert_eq!(engine.agent(&name).unwrap().run_state, "working");
        assert!(events(&engine, "run_end").is_empty());
    }

    #[test]
    fn typing_into_a_herdr_pane_counts_as_a_run() {
        let (engine, name, dir) = engine_with_herdr_agent("typed", |_| {});
        std::fs::write(dir.join("state"), "working").unwrap();
        engine.refresh_herdr().unwrap();
        let agent = engine.agent(&name).unwrap();
        assert_eq!((agent.run_state.as_str(), agent.runs), ("working", 1));
        assert_eq!(events(&engine, "run_start").len(), 1);
    }

    #[test]
    fn a_herdr_run_ends_when_it_stops_or_its_pane_goes() {
        for (case, stop) in [("gone", false), ("stop", true)] {
            let (engine, name, dir) = engine_with_herdr_agent(case, |_| {});
            status_line(&name, "s1", 1.0);
            engine.prompt(&name, "do it").unwrap();
            status_line(&name, "s1", 1.25);
            if stop {
                std::fs::write(dir.join("state"), "working").unwrap();
                engine.stop(&name).unwrap();
            } else {
                engine.refresh_herdr().unwrap();
            }
            assert_eq!(engine.agent(&name).unwrap().run_state, "stopped", "{case}");
            let ends = events(&engine, "run_end");
            assert_eq!(ends.len(), 1, "{case}: {ends:?}");
            assert_eq!(ends[0]["state"], "stopped", "{case}");
            assert_eq!(ends[0]["cost_usd"], 0.25, "{case}");
        }
    }

    #[test]
    fn a_herdr_build_agent_is_asked_for_a_checkpoint() {
        for (case, stage, total, limit_tokens, asked) in [
            ("cost", "build", 20.0, 0, true),
            ("tokens", "build", 0.5, 1000, true),
            ("under", "build", 0.5, 2000, false),
            ("research", "research", 20.0, 1000, false),
        ] {
            let (mut engine, name, dir) =
                engine_with_herdr_agent(&format!("cp-{case}"), |a| a.stage = stage.into());
            engine.cfg.stages.build_checkpoint_tokens = limit_tokens;
            status_line(&name, "s1", 0.0);
            engine.prompt(&name, "do it").unwrap();
            status_line(&name, "s1", total);
            std::fs::write(dir.join("state"), "idle").unwrap();
            settle_run(&engine, &name);
            engine.refresh_herdr().unwrap();
            assert_eq!(
                events(&engine, "checkpoint_due").len() == 1,
                asked,
                "{case}"
            );
            let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
            assert_eq!(
                calls.matches("agent prompt").count() == 2,
                asked,
                "{case}: {calls}"
            );
        }
    }

    #[test]
    fn a_fresh_herdr_session_replaces_the_pane_and_session() {
        let (engine, name, dir) = engine_with_herdr_agent("fresh", |a| a.stage = "build".into());
        std::fs::write(engine.agent_dir(&name).join("fresh"), "").unwrap();
        engine.prompt(&name, "start from the checkpoint").unwrap();
        let agent = engine.agent(&name).unwrap();
        assert_ne!(agent.session_id, "s1");
        assert_eq!(agent.pane, "w1:p2");
        assert_eq!(agent.run_state, "working");
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(calls.contains("pane close w1:p1"), "{calls}");
        assert!(calls.contains("--session-id"), "{calls}");
        assert!(calls.contains("agent prompt w1:p2"), "{calls}");
    }

    #[test]
    fn reads_only_what_a_log_added() {
        let dir = std::env::temp_dir().join(format!("drovr-runlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.jsonl");
        let text = |t: &str| {
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": t }] } })
                .to_string()
        };
        let shown = |p: &Path| {
            rendered_log(p, &harness::CLAUDE)
                .iter()
                .map(|r| match r {
                    Rendered::Line(l) => l.text.clone(),
                    Rendered::Result(_) => "result".into(),
                })
                .collect::<Vec<_>>()
        };
        std::fs::write(&path, format!("{}\n{}", text("a"), &text("b")[..10])).unwrap();
        // The second line is still being written.
        assert_eq!(shown(&path), ["a"]);
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", &text("b")[10..]).unwrap();
        writeln!(
            f,
            r#"{{"type":"result","total_cost_usd":0.5,"session_id":"s"}}"#
        )
        .unwrap();
        assert_eq!(shown(&path), ["a", "b", "result"]);
        // A complete last line without a newline counts.
        write!(f, "{}", text("c")).unwrap();
        assert_eq!(shown(&path), ["a", "b", "result", "c"]);
        // A new file at the same path (an agent removed and made again).
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, format!("{}\n", text("new"))).unwrap();
        assert_eq!(shown(&path), ["new"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
