//! The coding-agent CLIs drovr runs agents with ("harnesses"). Everything
//! that depends on one CLI's flags or output is behind [`Harness`]: building
//! its commands, reading its run logs and costs, finding its saved sessions,
//! and attaching to one. The rest of drovr (the database, the coordination
//! MCP server, the Inbox, reports, the UI) works the same whichever runs an
//! agent. Adding a CLI means one more implementation, here.

mod claude;
mod codex;

pub use claude::Claude;
pub use codex::Codex;

use crate::config::Config;
use crate::engine::Line;
use crate::sessions::SavedSession;
use crate::store::Agent;
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Claude Code, the default.
pub static CLAUDE: Claude = Claude;
/// OpenAI's Codex CLI (experimental).
pub static CODEX: Codex = Codex;

/// Every harness, the default first.
pub fn all() -> [&'static dyn Harness; 2] {
    [&CLAUDE, &CODEX]
}

/// A harness by name; empty means the default (Claude Code).
pub fn named(name: &str) -> Result<&'static dyn Harness> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(&CLAUDE);
    }
    match all().into_iter().find(|h| h.name() == name) {
        Some(h) => Ok(h),
        None => bail!("unknown harness {name:?}: use claude or codex"),
    }
}

/// The model an agent runs on: its own (a stage's, or one you chose), else
/// the harness's configured default; empty for the CLI's own default.
pub fn model_of(h: &dyn Harness, agent: &Agent, cfg: &Config) -> String {
    if agent.model.is_empty() {
        h.model(cfg).to_string()
    } else {
        agent.model.clone()
    }
}

/// An agent's harness. Agents stored before harnesses existed have none:
/// they're Claude Code agents.
pub fn of(agent: &Agent) -> &'static dyn Harness {
    named(&agent.harness).unwrap_or(&CLAUDE)
}

/// One coding-agent CLI.
pub trait Harness: Sync {
    /// Its name in the config and the database: `claude`, `codex`.
    fn name(&self) -> &'static str;

    /// The CLI to run, from the config.
    fn command<'a>(&self, cfg: &'a Config) -> &'a str;

    /// The model it's configured to use; empty for the CLI's own default.
    fn model<'a>(&self, cfg: &'a Config) -> &'a str;

    /// Whether drovr picks the session ID before the first run. When it
    /// doesn't, the run reports one (`Event::Session`) and drovr keeps it.
    fn picks_session_id(&self) -> bool;

    // -- building the command -------------------------------------------------

    /// Arguments for a headless run, which reads its prompt from stdin and
    /// writes JSON lines to stdout.
    fn headless_args(&self, run: &Run) -> Result<Vec<String>>;

    /// Whether agents can run in herdr panes.
    fn runs_in_herdr(&self) -> bool {
        false
    }

    /// Arguments for starting the agent in a herdr pane.
    fn herdr_args(&self, run: &Run) -> Result<Vec<String>> {
        let _ = run;
        bail!(
            "{} agents run headless for now, not in herdr panes",
            self.label()
        )
    }

    // -- reading a run --------------------------------------------------------

    /// What one line of a run log means.
    fn parse(&self, event: &Value) -> Event;

    /// What the run that ended with `end` (an `Event::End`) cost, in US
    /// dollars, or None when the CLI doesn't report a cost. `costs` carries
    /// what earlier runs of the agent reported, in order.
    fn run_cost(&self, end: &Value, costs: &mut Costs) -> Option<f64> {
        let _ = (end, costs);
        None
    }

    /// How much context the session carried at the end of a run's `log`,
    /// in tokens, or None when the CLI doesn't say.
    fn context_tokens(&self, log: &str) -> Option<u64> {
        let _ = log;
        None
    }

    // -- finding sessions -----------------------------------------------------

    /// The CLI's own transcript of a session, which Code review and the
    /// terminal read for the folders an agent worked in.
    fn session_file(&self, id: &str) -> Option<PathBuf> {
        let _ = id;
        None
    }

    /// A saved session by ID, to start an agent from.
    fn find_session(&self, id: &str) -> Result<SavedSession> {
        let _ = id;
        bail!(
            "{} agents can't start from an existing session",
            self.label()
        )
    }

    /// The most recently used saved sessions, newest first.
    fn recent_sessions(&self, limit: usize) -> Vec<SavedSession> {
        let _ = limit;
        vec![]
    }

    // -- attaching ------------------------------------------------------------

    /// The command line (without a `cd`) that opens the agent's session
    /// interactively, already shell-quoted.
    fn attach_command(&self, run: &Run) -> Result<String>;

    /// How the UI and messages name it.
    fn label(&self) -> &'static str;
}

/// Whether a run starts a session, continues the agent's, or continues a
/// copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    New,
    Resume,
    Fork,
}

/// What starting a run needs.
pub struct Run<'a> {
    pub cfg: &'a Config,
    pub agent: &'a Agent,
    /// drovr's CLI, which is each agent's MCP server.
    pub cli: PathBuf,
    /// drovr's data folder, passed to the MCP server.
    pub home: PathBuf,
    /// The MCP config file drovr wrote for the agent (Claude Code reads it).
    pub mcp_config: PathBuf,
    /// The system prompt drovr wrote for the agent.
    pub system_prompt: PathBuf,
    pub session: Session,
}

/// What one line of a run log means.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Lines to show in the agent's output.
    Lines(Vec<Line>),
    /// The session this run belongs to.
    Session(String),
    /// The run ended. `{is_error, session_id, total_cost_usd, duration_ms,
    /// num_turns, input_tokens, output_tokens, tokens: {input, output,
    /// cache_read, cache_write}, models: [..]}`, each null when the harness
    /// doesn't report it. `input_tokens`/`output_tokens` are shown in the
    /// run's last line; `tokens` and `models` go to the record.
    End(Value),
    /// Nothing to show.
    Skip,
}

/// What each session reported in total so far, for CLIs that report a
/// running total (Claude Code's `total_cost_usd` covers the whole session,
/// and every resumed run reports it again).
#[derive(Default)]
pub struct Costs(HashMap<String, f64>);

impl Costs {
    /// What a run added to its session's running `total`.
    pub fn added(&mut self, session: &str, total: f64) -> f64 {
        match self.0.insert(session.to_string(), total) {
            // A lower total means a new count (a new or forked session).
            Some(before) if total >= before => total - before,
            _ => total,
        }
    }
}

fn line(kind: &str, text: String) -> Line {
    Line {
        kind: kind.into(),
        text,
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn agent(session: &str) -> Agent {
        Agent {
            name: "api".into(),
            session_id: session.into(),
            ..Agent::default()
        }
    }

    pub fn with<T>(a: &Agent, session: Session, f: impl FnOnce(&Run) -> T) -> T {
        // Each call its own file: tests run in parallel, and a shared one
        // could be read while another test is rewriting it.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("drovr-harness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = dir.join(format!("system-{n}.md"));
        std::fs::write(&system, "You are \"api\".\nLine two.").unwrap();
        let cfg = Config::default();
        let run = Run {
            cfg: &cfg,
            agent: a,
            cli: "/opt/drovr".into(),
            home: "/data/drovr".into(),
            mcp_config: "/data/drovr/agents/api/mcp.json".into(),
            system_prompt: system,
            session,
        };
        f(&run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_harnesses_by_name() {
        assert_eq!(named("").unwrap().name(), "claude");
        assert_eq!(named("codex").unwrap().name(), "codex");
        assert!(named("gemini").is_err());
        let old = Agent::default();
        assert_eq!(of(&old).name(), "claude");
    }

    #[test]
    fn counts_each_runs_own_cost() {
        let mut c = Costs::default();
        let r = |c: &mut Costs, s: &str, t: f64| {
            CLAUDE
                .run_cost(
                    &serde_json::json!({ "session_id": s, "total_cost_usd": t }),
                    c,
                )
                .unwrap()
        };
        // Resumed runs report the session's running total.
        assert!((r(&mut c, "a", 0.71) - 0.71).abs() < 1e-9);
        assert!((r(&mut c, "a", 0.80) - 0.09).abs() < 1e-9);
        assert!((r(&mut c, "a", 6.49) - 5.69).abs() < 1e-9);
        // A different session counts from its own start.
        assert!((r(&mut c, "b", 1.00) - 1.00).abs() < 1e-9);
        assert!((r(&mut c, "a", 6.50) - 0.01).abs() < 1e-9);
        // A total that went down started over.
        assert!((r(&mut c, "a", 0.20) - 0.20).abs() < 1e-9);
        // Codex reports tokens, not a cost.
        assert_eq!(
            CODEX.run_cost(&serde_json::json!({ "total_cost_usd": null }), &mut c),
            None
        );
    }
}
