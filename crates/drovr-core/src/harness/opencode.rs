//! OpenCode 2 (`opencode`). Each prompt is an `opencode run --format json`
//! run. drovr's MCP server and system prompt reach it through a config file
//! drovr writes for the agent (`OPENCODE_CONFIG`), which OpenCode merges over
//! your own, and drovr's permission prompt through a plugin in that config:
//! before each tool call it asks `drovr hook permission`, which answers the
//! same way Claude Code's permission prompt is answered (`permissions.*`,
//! then the Inbox). Agents can also run in herdr panes (herdr's `opencode`
//! kind), where you answer OpenCode's own prompts; sessions can be adopted,
//! and forked headless.
//!
//! OpenCode 2 runs sessions in a background service that was started with
//! its own environment, so it would never read the agent's config. Every
//! start passes `--standalone`: a server of its own, started with drovr's
//! environment. Saved sessions, costs and models are read from OpenCode's
//! database, read-only.

use super::{line, Costs, Event, Harness, Live, Run, Session, Transcript};
use crate::config::Config;
use crate::engine::truncate;
use crate::sessions::SavedSession;
use crate::shell::quote;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub struct OpenCode;

/// The config a headless run reads; the plugin (a folder) it loads.
const RUN_CONFIG: &str = "opencode-run.json";
const PLUGIN_DIR: &str = "opencode-plugin";
/// The config an interactive session (attach, a herdr pane) reads: no
/// plugin, since you answer its permission prompts yourself.
const ATTACH_CONFIG: &str = "opencode.json";

impl Harness for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn label(&self) -> &'static str {
        "OpenCode"
    }

    fn command<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.opencode.command
    }

    fn model<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.opencode.model
    }

    /// OpenCode names the session when the first run starts.
    fn picks_session_id(&self) -> bool {
        false
    }

    fn headless_args(&self, run: &Run) -> Result<Vec<String>> {
        // The prompt comes from stdin. OpenCode's own prompts are answered
        // by drovr's plugin before a tool runs, so `--auto` only lets
        // through what drovr allowed; your config's `deny` rules still hold.
        let mut args: Vec<String> = ["run", "--standalone", "--format", "json", "--auto"]
            .map(String::from)
            .to_vec();
        let id = &run.agent.session_id;
        match run.session {
            Session::New => {}
            Session::Resume => args.extend(["--session".into(), id.clone()]),
            Session::Fork => args.extend(["--session".into(), id.clone(), "--fork".into()]),
        }
        args.extend(opencode_common(run));
        Ok(args)
    }

    fn headless_env(&self, run: &Run) -> Result<Vec<(String, String)>> {
        // OpenCode 2 loads a configured plugin from a folder: `server.js`,
        // an ES module (its package.json says so).
        let plugin = run.dir.join(PLUGIN_DIR);
        std::fs::create_dir_all(&plugin)?;
        std::fs::write(
            plugin.join("package.json"),
            r#"{ "name": "drovr-permissions", "private": true, "type": "module" }"#,
        )?;
        std::fs::write(plugin.join("server.js"), plugin_source(run))?;
        let mut cfg = config(run);
        cfg["plugins"] = json!([plugin.display().to_string()]);
        let path = run.dir.join(RUN_CONFIG);
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![
            ("OPENCODE_CONFIG".into(), path.display().to_string()),
            // `--pure` (or this) would skip the plugin, and with it every
            // permission check.
            ("OPENCODE_PURE".into(), String::new()),
            // OpenCode takes the session's folder from $PWD.
            ("PWD".into(), run.agent.cwd.clone()),
        ])
    }

    fn parse(&self, v: &Value, run: &mut Value) -> Event {
        opencode_event(v, run)
    }

    /// Each run reports what it cost itself.
    fn run_cost(&self, end: &Value, costs: &mut Costs) -> Option<f64> {
        let _ = costs;
        end["cost_usd"].as_f64()
    }

    fn context_tokens(&self, log: &str) -> Option<u64> {
        let mut state = Value::Null;
        log.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| match self.parse(&v, &mut state) {
                Event::End(end) => end["context_tokens"].as_u64(),
                _ => None,
            })
            .last()
    }

    fn find_session(&self, id: &str) -> Result<SavedSession> {
        if !valid_id(id) {
            bail!("{id:?} isn't an OpenCode session ID (ses_…)");
        }
        let cfg = Config::load().unwrap_or_default();
        saved_sessions(&open_db(&cfg)?, Some(id), 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no OpenCode session {id} (see `opencode session list`)"))
    }

    fn recent_sessions(&self, limit: usize) -> Vec<SavedSession> {
        let cfg = Config::load().unwrap_or_default();
        open_db(&cfg)
            .and_then(|db| saved_sessions(&db, None, limit))
            .unwrap_or_default()
    }

    fn runs_in_herdr(&self) -> bool {
        true
    }

    /// OpenCode's TUI. Until drovr knows the session a pane started (see
    /// `latest_session`), resuming starts a new one rather than guessing.
    /// The TUI takes no model or agent flags: they're in its config.
    fn herdr_args(&self, run: &Run) -> Result<Vec<String>> {
        let id = &run.agent.session_id;
        let mut args = vec!["--standalone".to_string()];
        match run.session {
            _ if id.is_empty() => {}
            Session::New => {}
            Session::Resume => args.extend(["--session".into(), id.clone()]),
            Session::Fork => bail!(
                "OpenCode's TUI can't fork a session: run {} headless to fork it",
                run.agent.name
            ),
        }
        Ok(args)
    }

    fn herdr_env(&self, run: &Run) -> Result<Vec<(String, String)>> {
        Ok(vec![(
            "OPENCODE_CONFIG".into(),
            attach_config(run)?.display().to_string(),
        )])
    }

    /// What the session has cost (its subagents' sessions included), the
    /// context its last answer was given and the model, from OpenCode's
    /// database. herdr agents are refreshed every few seconds, so answers
    /// are kept for a few.
    fn session_live(&self, cfg: &Config, id: &str) -> Option<Live> {
        if !valid_id(id) {
            return None;
        }
        static SEEN: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
        let seen = SEEN.get_or_init(Default::default);
        if let Some((at, live)) = seen.lock().ok()?.get(id) {
            if at.elapsed() < Duration::from_secs(8) {
                return live.clone();
            }
        }
        let live = open_db(cfg)
            .and_then(|db| live_session(&db, id))
            .ok()
            .flatten();
        if let Ok(mut s) = seen.lock() {
            s.insert(id.to_string(), (Instant::now(), live.clone()));
        }
        live
    }

    /// The session's messages, from OpenCode's database, as the events
    /// `opencode run --format json` prints, with your prompts as `prompt`
    /// events. The app asks every second or two, so answers are kept for two.
    fn transcript(&self, cfg: &Config, id: &str) -> Option<Transcript> {
        if !valid_id(id) {
            return None;
        }
        static SEEN: OnceLock<Mutex<HashMap<String, Read>>> = OnceLock::new();
        let seen = SEEN.get_or_init(Default::default);
        if let Some((at, events)) = seen.lock().ok()?.get(id) {
            if at.elapsed() < Duration::from_secs(2) {
                return Some(Transcript::Events(events.clone()));
            }
        }
        // The newest parts (their IDs sort by time), oldest first.
        let sql = format!(
            "SELECT * FROM (SELECT p.id, p.data AS part, json_extract(m.data, '$.role') AS role, \
               p.time_created AS t FROM part p JOIN message m ON m.id = p.message_id \
               WHERE p.session_id = '{id}' ORDER BY p.message_id DESC, p.id DESC LIMIT {TRANSCRIPT_PARTS}) \
             ORDER BY 1"
        );
        let events = transcript_events(id, &db(cfg, &sql).ok()?);
        if let Ok(mut s) = seen.lock() {
            s.insert(id.to_string(), (Instant::now(), events.clone()));
        }
        Some(Transcript::Events(events))
    }

    fn parse_transcript(&self, v: &Value, run: &mut Value) -> Event {
        // Each prompt starts a turn, whose figures are counted afresh.
        if v["type"] == "prompt" {
            *run = Value::Null;
            opencode_event(&json!({ "timestamp": v["timestamp"] }), run);
            return Event::Lines(vec![line(
                "prompt",
                v["text"].as_str().unwrap_or("").into(),
            )]);
        }
        opencode_event(v, run)
    }

    fn latest_session(&self, cfg: &Config, cwd: &str, since: f64) -> Option<String> {
        open_db(cfg)
            .and_then(|db| newest_session(&db, cwd, since))
            .ok()
            .flatten()
    }

    fn attach_command(&self, run: &Run) -> Result<String> {
        let id = &run.agent.session_id;
        if id.is_empty() {
            bail!("{} hasn't started an OpenCode session yet", run.agent.name);
        }
        let path = attach_config(run)?;
        let args = [
            self.command(run.cfg).to_string(),
            "--standalone".into(),
            "--session".into(),
            id.clone(),
        ];
        Ok(format!(
            "OPENCODE_CONFIG={} {}",
            quote(&path.display().to_string()),
            args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        ))
    }
}

/// A headless run's model, OpenCode agent and your extra arguments.
fn opencode_common(run: &Run) -> Vec<String> {
    let cfg = run.cfg;
    let mut args = vec![];
    let model = super::model_of(&OpenCode, run.agent, cfg);
    if !model.is_empty() {
        args.extend(["-m".into(), model]);
    }
    if !cfg.opencode.agent.is_empty() {
        args.extend(["--agent".into(), cfg.opencode.agent.clone()]);
    }
    args.extend(cfg.opencode.extra_args.iter().cloned());
    args
}

/// drovr's part of OpenCode's config: its MCP server (whose tools can wait
/// for you for many minutes, so with a long timeout), the system prompt as
/// an instructions file, and the model and OpenCode agent, which the TUI
/// takes only from its config.
fn config(run: &Run) -> Value {
    let mut cfg = json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": {
            "drovr": {
                "type": "local",
                "command": [run.cli.display().to_string(), "mcp", "--agent", run.agent.name],
                "environment": {
                    "DROVR_HOME": run.home.display().to_string(),
                    "DROVR_AGENT": run.agent.name,
                },
                "enabled": true,
                "timeout": 3_600_000,
            }
        },
        "instructions": [run.system_prompt.display().to_string()],
    });
    let model = super::model_of(&OpenCode, run.agent, run.cfg);
    if !model.is_empty() {
        cfg["model"] = json!(model);
    }
    if !run.cfg.opencode.agent.is_empty() {
        cfg["default_agent"] = json!(run.cfg.opencode.agent);
    }
    // OpenCode adds every config file's rules together, and the last match
    // wins: this holds in a herdr pane too, where there's no plugin.
    if super::read_only(run.agent) {
        cfg["permissions"] = json!([{ "action": "edit", "resource": "*", "effect": "deny" }]);
    }
    cfg
}

/// The plugin that asks drovr before each tool call, in OpenCode 2's plugin
/// API (`{id, setup}`, hooks registered on `ctx.tool`). It runs under Node
/// or Bun, so it starts drovr with `node:child_process`. A denial throws,
/// which OpenCode hands the model as the tool's error; so does any failure
/// to ask, so a broken check never lets a call through.
fn plugin_source(run: &Run) -> String {
    let js = |s: &str| Value::String(s.to_string()).to_string();
    format!(
        r#"// Written by drovr for agent {name}: asks drovr before each tool call,
// as Claude Code's permission prompt does. Rewritten before every run.
import {{ spawn }} from "node:child_process";

const CLI = {cli};
const AGENT = {agent};
const HOME = {home};

// MCP tools are `<server>_<tool>`, and a server's name can have `_` in it:
// drovr is told the servers' names to split them at the right place.
async function mcpServers(ctx) {{
  try {{
    const res = await ctx.mcp.list();
    const list = Array.isArray(res) ? res : res?.data;
    return Array.isArray(list) ? list.map((s) => s?.name).filter((n) => typeof n === "string") : [];
  }} catch {{
    return [];
  }}
}}

function ask(req) {{
  return new Promise((resolve, reject) => {{
    const proc = spawn(CLI, ["hook", "permission", AGENT], {{
      env: {{ ...process.env, DROVR_HOME: HOME, DROVR_AGENT: AGENT }},
      stdio: ["pipe", "pipe", "pipe"],
    }});
    let out = "";
    let err = "";
    proc.stdout.on("data", (d) => (out += d));
    proc.stderr.on("data", (d) => (err += d));
    proc.on("error", reject);
    proc.on("close", () => resolve({{ out, err }}));
    proc.stdin.end(JSON.stringify(req));
  }});
}}

export default {{
  id: "drovr-permissions",
  setup: async (ctx) => {{
    await ctx.tool.hook("execute.before", async (event) => {{
      const req = {{ tool: event.tool, args: event.input ?? {{}}, servers: await mcpServers(ctx) }};
      const {{ out, err }} = await ask(req);
      let answer;
      try {{
        answer = JSON.parse(out);
      }} catch {{
        throw new Error(`drovr couldn't check this tool call: ${{(err || out).trim()}}`);
      }}
      if (answer.behavior !== "allow") throw new Error(answer.message || "Denied.");
    }});
  }},
}};
"#,
        name = run.agent.name,
        cli = js(&run.cli.display().to_string()),
        agent = js(&run.agent.name),
        home = js(&run.home.display().to_string()),
    )
}

/// How many of a session's newest parts the native view of a herdr agent
/// reads.
const TRANSCRIPT_PARTS: usize = 2000;

/// Part rows (`{part, role, t}`, the part as JSON text) as `run --format
/// json` events: finished text and tool calls, steps, and the prompts you
/// typed (a user message's text that OpenCode didn't add itself).
fn transcript_events(id: &str, rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| {
            let part: Value = serde_json::from_str(r["part"].as_str()?).ok()?;
            let t = &r["t"];
            let event =
                |kind: &str| json!({ "type": kind, "timestamp": t, "sessionID": id, "part": part });
            match (r["role"].as_str()?, part["type"].as_str()?) {
                ("user", "text") if part["synthetic"] != true && part["ignored"] != true => {
                    Some(json!({ "type": "prompt", "timestamp": t, "text": part["text"] }))
                }
                ("assistant", "text") if !part["time"]["end"].is_null() => Some(event("text")),
                ("assistant", "tool")
                    if matches!(
                        part["state"]["status"].as_str(),
                        Some("completed" | "error")
                    ) =>
                {
                    Some(event("tool_use"))
                }
                ("assistant", "step-start") => Some(event("step_start")),
                ("assistant", "step-finish") => Some(event("step_finish")),
                _ => None,
            }
        })
        .collect()
}

/// A session's transcript events and when they were read.
type Read = (Instant, Vec<Value>);

/// A session's figures and when they were read.
type Seen = (Instant, Option<Live>);

/// The config an interactive session reads, written fresh.
fn attach_config(run: &Run) -> Result<PathBuf> {
    std::fs::create_dir_all(&run.dir)?;
    let path = run.dir.join(ATTACH_CONFIG);
    std::fs::write(&path, serde_json::to_string_pretty(&config(run))?)?;
    Ok(path)
}

/// Whether `id` looks like an OpenCode session ID (`ses_…`).
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Where OpenCode keeps its database, as `opencode debug paths db` says (it
/// depends on OpenCode's release channel and `OPENCODE_DB`). Asked once
/// for each command.
fn db_path(cfg: &Config) -> Result<PathBuf> {
    static PATHS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let paths = PATHS.get_or_init(Default::default);
    let command = &cfg.opencode.command;
    if let Some(p) = paths.lock().ok().and_then(|p| p.get(command).cloned()) {
        return Ok(p);
    }
    let out = Command::new(command)
        .args(["debug", "paths", "db"])
        .output()
        .with_context(|| format!("running {command}"))?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || !Path::new(&path).is_absolute() {
        bail!(
            "`{command} debug paths db` didn't name OpenCode's database (drovr needs OpenCode 2): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let path = PathBuf::from(path);
    if let Ok(mut p) = paths.lock() {
        p.insert(command.clone(), path.clone());
    }
    Ok(path)
}

/// OpenCode's database, read-only.
fn open_db(cfg: &Config) -> Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let path = db_path(cfg)?;
    let db = rusqlite::Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    db.busy_timeout(Duration::from_secs(2))?;
    Ok(db)
}

/// Saved sessions, newest first: one by ID, or the most recent. Subagents'
/// sessions (with a parent) and archived ones are left out.
fn saved_sessions(
    db: &rusqlite::Connection,
    id: Option<&str>,
    limit: usize,
) -> Result<Vec<SavedSession>> {
    let mut q = db.prepare(
        "SELECT id, title, directory, time_updated FROM session_v2 \
         WHERE parent_id IS NULL AND time_archived IS NULL AND (?1 IS NULL OR id = ?1) \
         ORDER BY time_updated DESC LIMIT ?2",
    )?;
    let rows = q.query_map(rusqlite::params![id, limit as i64], |r| {
        Ok(SavedSession {
            id: r.get(0)?,
            first_prompt: truncate(&r.get::<_, Option<String>>(1)?.unwrap_or_default(), 200),
            cwd: r.get(2)?,
            modified: r.get::<_, i64>(3)? as f64 / 1000.0,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A session's figures: what it and its subagents' sessions have cost, and
/// the context and model of its last answer. None for an unknown session.
fn live_session(db: &rusqlite::Connection, id: &str) -> Result<Option<Live>> {
    use rusqlite::OptionalExtension;
    let cost: Option<f64> = db.query_row(
        "SELECT SUM(cost) FROM session_v2 WHERE id = ?1 OR parent_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let Some(cost) = cost else {
        return Ok(None);
    };
    let last: Value = db
        .query_row(
            "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'assistant' \
             ORDER BY seq DESC LIMIT 1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let t = &last["tokens"];
    let n = |x: &Value| x.as_u64().unwrap_or(0);
    Ok(Some(Live {
        session: id.to_string(),
        total_cost_usd: cost,
        context_tokens: n(&t["input"]) + n(&t["cache"]["read"]) + n(&t["cache"]["write"]),
        model: last["model"]["id"].as_str().unwrap_or("").to_string(),
    }))
}

/// The newest session (not a subagent's) started in `cwd` since `since`
/// (Unix seconds).
fn newest_session(db: &rusqlite::Connection, cwd: &str, since: f64) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(db
        .query_row(
            "SELECT id FROM session_v2 WHERE parent_id IS NULL AND directory = ?1 \
             AND time_created >= ?2 ORDER BY time_created DESC LIMIT 1",
            rusqlite::params![cwd, (since * 1000.0) as i64],
            |r| r.get(0),
        )
        .optional()?)
}

/// `opencode run --format json`: `{type, timestamp, sessionID, part}` for
/// each finished step (`step_start`, `step_finish` with that step's cost and
/// tokens), text and tool call, and `{type: "error", error}`. There's no
/// line for the end of the run, so the step that stops (a `step_finish` that
/// isn't for tool calls) ends it, with the run's totals kept in `run`.
fn opencode_event(v: &Value, run: &mut Value) -> Event {
    if !run.is_object() {
        *run = json!({
            "start": v["timestamp"],
            "cost": 0.0,
            "steps": 0,
            "input": 0, "output": 0, "reasoning": 0, "cache_read": 0, "cache_write": 0,
            "error": false,
        });
    }
    let part = &v["part"];
    match v["type"].as_str() {
        Some("step_start") => match v["sessionID"].as_str() {
            Some(id) => Event::Session(id.to_string()),
            None => Event::Skip,
        },
        Some("text") => match part["text"].as_str().map(str::trim) {
            Some(t) if !t.is_empty() => Event::Lines(vec![line("text", t.to_string())]),
            _ => Event::Skip,
        },
        Some("tool_use") => {
            let state = &part["state"];
            let input = &state["input"];
            let summary = [
                "command",
                "path",
                "filePath",
                "pattern",
                "url",
                "query",
                "to",
                "question",
                "description",
            ]
            .iter()
            .find_map(|k| input[*k].as_str().map(String::from))
            .unwrap_or_else(|| truncate(&input.to_string(), 200));
            let mut out = vec![line(
                "tool",
                format!(
                    "{} {}",
                    part["tool"].as_str().unwrap_or("tool"),
                    truncate(&summary, 300)
                ),
            )];
            if state["status"] == "error" {
                out.push(line(
                    "error",
                    truncate(state["error"].as_str().unwrap_or(""), 600),
                ));
            } else {
                out.push(line(
                    "result",
                    truncate(state["output"].as_str().unwrap_or(""), 600),
                ));
            }
            Event::Lines(out)
        }
        Some("error") => {
            run["error"] = json!(true);
            // OpenCode 2: `{type, message}`; 1: `{name, data: {message}}`.
            let e = &v["error"];
            let text = e["message"]
                .as_str()
                .or(e["data"]["message"].as_str())
                .or(e["name"].as_str())
                .map(String::from)
                .unwrap_or_else(|| e.to_string());
            Event::Lines(vec![line("error", truncate(&text, 600))])
        }
        Some("step_finish") => {
            let t = &part["tokens"];
            let add = |run: &mut Value, key: &str, n: &Value| {
                run[key] = json!(run[key].as_u64().unwrap_or(0) + n.as_u64().unwrap_or(0));
            };
            run["cost"] =
                json!(run["cost"].as_f64().unwrap_or(0.0) + part["cost"].as_f64().unwrap_or(0.0));
            add(run, "steps", &json!(1));
            add(run, "input", &t["input"]);
            add(run, "output", &t["output"]);
            add(run, "reasoning", &t["reasoning"]);
            add(run, "cache_read", &t["cache"]["read"]);
            add(run, "cache_write", &t["cache"]["write"]);
            // Everything this step's model call was given.
            let n = |x: &Value| x.as_u64().unwrap_or(0);
            run["context"] =
                json!(n(&t["input"]) + n(&t["cache"]["read"]) + n(&t["cache"]["write"]));
            if matches!(part["reason"].as_str(), Some("tool-calls" | "unknown")) {
                return Event::Skip;
            }
            let duration = match (v["timestamp"].as_f64(), run["start"].as_f64()) {
                (Some(end), Some(start)) => json!(end - start),
                _ => Value::Null,
            };
            Event::End(json!({
                "is_error": run["error"] == true || part["reason"] == "error",
                "session_id": v["sessionID"],
                // OpenCode reports each step's cost, not a running total.
                "total_cost_usd": null,
                "cost_usd": run["cost"],
                "duration_ms": duration,
                "num_turns": run["steps"],
                "input_tokens": null,
                "output_tokens": null,
                "tokens": {
                    "input": run["input"],
                    "output": json!(n(&run["output"]) + n(&run["reasoning"])),
                    "cache_read": run["cache_read"],
                    "cache_write": run["cache_write"],
                },
                "context_tokens": run["context"],
                "models": null,
            }))
        }
        _ => Event::Skip,
    }
}

// -- permissions --------------------------------------------------------------

/// What drovr's plugin asks before a tool runs: `{tool, args}` with
/// OpenCode's tool name and arguments, asked as the Claude Code tool it
/// matches so `permissions.auto_allow`, `defaults.disallowed_tools`,
/// `defaults.permission_mode` and the Inbox treat it the same. Returns
/// `{behavior: "allow"}` or `{behavior: "deny", message}`.
pub fn permission(engine: crate::Engine, me: &str, req: &Value) -> Result<Value> {
    let tool = req["tool"].as_str().unwrap_or("");
    let servers: Vec<String> = req["servers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let name = claude_name(tool, &servers);
    let input = claude_input(&req["args"]);
    let cfg = &engine.cfg;
    let agent = engine.agent(me)?;
    if super::disallowed(cfg, &agent).contains(&name) {
        let message = if super::read_only(&agent) {
            format!("{name} isn't allowed: you're a reviewer and can't change files. Send the change to your builder as a proposed diff.")
        } else {
            format!("{name} isn't allowed for drovr agents.")
        };
        return Ok(json!({ "behavior": "deny", "message": message }));
    }
    match settled(cfg, &agent.cwd, &name, &input) {
        Some(answer) => Ok(answer),
        None => crate::mcp::Server::new(engine, me.to_string())
            .call("approve", &json!({ "tool_name": name, "input": input })),
    }
}

/// An answer that needs no one: tools Claude Code runs without asking, and
/// what the permission mode allows. None goes to the permission prompt.
fn settled(cfg: &Config, cwd: &str, name: &str, input: &Value) -> Option<Value> {
    let allow = Some(json!({ "behavior": "allow" }));
    // Never asked about in Claude Code: subagents, to-do lists, skills and
    // questions. CodeMode is OpenCode's interpreter, which reaches the
    // outside world only through tools: each tool it calls is asked about
    // on its own.
    if matches!(
        name,
        "Task" | "TodoRead" | "TodoWrite" | "Skill" | "AskUserQuestion" | "CodeMode"
    ) {
        return allow;
    }
    let edit = matches!(name, "Edit" | "Write" | "MultiEdit");
    match cfg.defaults.permission_mode.as_str() {
        "bypassPermissions" => allow,
        "plan" if edit => Some(json!({
            "behavior": "deny",
            "message": "This agent runs in plan mode: it can't change files.",
        })),
        // Claude Code's `auto` judges each call itself; drovr has no such
        // judge, so for OpenCode it's `acceptEdits`, and the rest is asked.
        // The plans folder is drovr's, where agents keep their plans and
        // reports, so edits there go through too.
        "acceptEdits" | "auto" if edit => {
            let plans = cfg.reports.path();
            let mut roots = vec![PathBuf::from(cwd)];
            // An empty path would hold everything.
            if plans.is_absolute() {
                roots.push(plans);
            }
            edits_inside(cwd, &roots, input).then(|| json!({ "behavior": "allow" }))
        }
        _ => None,
    }
}

/// Whether every file an edit touches (relative paths are from `cwd`) is
/// inside one of `roots`, as Claude Code's `acceptEdits` requires of the
/// working directory.
fn edits_inside(cwd: &str, roots: &[PathBuf], input: &Value) -> bool {
    let here = Path::new(cwd);
    let mut files: Vec<String> = input["file_path"]
        .as_str()
        .map(String::from)
        .into_iter()
        .collect();
    // apply_patch: `*** Update File: path` and the like.
    for l in input["patchText"].as_str().unwrap_or("").lines() {
        for head in [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ] {
            if let Some(f) = l.strip_prefix(head) {
                files.push(f.trim().to_string());
            }
        }
    }
    !files.is_empty()
        && files.iter().all(|f| {
            let p = normalize(&crate::review::resolve(here, f));
            roots.iter().any(|r| p.starts_with(normalize(r)))
        })
}

/// A path without `.` and `..`, so `cwd/../x` isn't counted as inside.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// OpenCode's tool as Claude Code names it (OpenCode 2's names, and 1's
/// where they differ). MCP tools are `<server>_<tool>` in OpenCode, with
/// the server's name sanitized: one is split after the longest of `servers`
/// (the MCP servers OpenCode has) it starts with, and drovr's own are told
/// apart exactly. Any other tool is OpenCode's own (`worktree_create`) and
/// keeps its name; only when the servers aren't known is it split at its
/// first `_`.
pub fn claude_name(tool: &str, servers: &[String]) -> String {
    let builtin = match tool {
        "shell" | "bash" => "Bash",
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "multiedit" => "MultiEdit",
        "patch" | "apply_patch" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "list" => "LS",
        "todowrite" => "TodoWrite",
        "todoread" => "TodoRead",
        "webfetch" => "WebFetch",
        "websearch" => "WebSearch",
        "subagent" | "task" => "Task",
        "skill" => "Skill",
        "question" => "AskUserQuestion",
        "execute" => "CodeMode",
        _ => "",
    };
    if !builtin.is_empty() {
        return builtin.into();
    }
    if let Some(rest) = tool.strip_prefix("drovr_") {
        return format!("mcp__drovr__{rest}");
    }
    let server = servers
        .iter()
        .map(|s| mcp_sanitize(s))
        .filter(|s| {
            tool.strip_prefix(s.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.starts_with('_'))
        })
        .max_by_key(|s| s.len());
    if let Some(server) = server {
        return format!("mcp__{server}__{}", &tool[server.len() + 1..]);
    }
    if !servers.is_empty() {
        return tool.to_string();
    }
    match tool.split_once('_') {
        Some((server, name)) => format!("mcp__{server}__{name}"),
        None => tool.to_string(),
    }
}

/// A name as OpenCode puts it in an MCP tool's name: anything but ASCII
/// letters, digits, `_` and `-` becomes `_`.
fn mcp_sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// OpenCode's arguments with Claude Code's names for the ones drovr reads
/// (`path` in OpenCode 2, `filePath` in 1, is `file_path`).
fn claude_input(args: &Value) -> Value {
    let mut input = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    if let Some(path) = input.get("path").or(input.get("filePath")).cloned() {
        input["file_path"] = path;
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{agent, with};

    #[test]
    fn opencode_args() {
        let a = agent("");
        let new = with(&a, Session::New, |r| OpenCode.headless_args(r).unwrap());
        // Its own server, which reads the agent's config.
        assert_eq!(new, ["run", "--standalone", "--format", "json", "--auto"]);
        let a = agent("ses_1");
        let resumed = with(&a, Session::Resume, |r| OpenCode.headless_args(r).unwrap());
        assert_eq!(resumed[5..], ["--session", "ses_1"]);
        let forked = with(&a, Session::Fork, |r| OpenCode.headless_args(r).unwrap());
        assert_eq!(forked[5..], ["--session", "ses_1", "--fork"]);
        let mut cfg = Config::default();
        cfg.opencode.model = "anthropic/claude-sonnet-5-5".into();
        cfg.opencode.agent = "build".into();
        let args = crate::harness::testing::with_config(&a, Session::Resume, cfg, |r| {
            OpenCode.headless_args(r).unwrap()
        });
        assert_eq!(
            args[7..],
            ["-m", "anthropic/claude-sonnet-5-5", "--agent", "build"]
        );
        assert!(with(&agent(""), Session::Resume, |r| OpenCode.attach_command(r)).is_err());
    }

    #[test]
    fn a_reviewer_is_refused_edits_whatever_the_mode() {
        let dir = std::env::temp_dir().join(format!("drovr-oc-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.defaults.permission_mode = "bypassPermissions".into();
        let engine = || {
            crate::Engine::with(
                crate::store::Store::open(&dir.join("t.db")).unwrap(),
                cfg.clone(),
                "drovr".into(),
            )
        };
        for (name, stage) in [("r", crate::config::REVIEW), ("b", "build")] {
            engine()
                .store
                .add_agent(&crate::store::Agent {
                    name: name.into(),
                    stage: stage.into(),
                    cwd: "/r/api".into(),
                    ..crate::store::Agent::default()
                })
                .unwrap();
        }
        let ask = |me: &str, tool: &str| {
            permission(
                engine(),
                me,
                &json!({ "tool": tool, "args": { "path": "/r/api/a.rs" } }),
            )
            .unwrap()
        };
        for tool in ["edit", "write", "patch"] {
            let answer = ask("r", tool);
            assert_eq!(answer["behavior"], "deny", "{tool}");
            assert!(answer["message"].as_str().unwrap().contains("reviewer"));
            assert_eq!(ask("b", tool)["behavior"], "allow", "{tool}");
        }
        assert_eq!(ask("r", "read")["behavior"], "allow");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_its_config_and_plugin() {
        let a = agent("ses_1");
        let (env, cmd, dir) = with(&a, Session::Resume, |r| {
            (
                OpenCode.headless_env(r).unwrap(),
                OpenCode.attach_command(r).unwrap(),
                r.dir.clone(),
            )
        });
        assert_eq!(env[0].0, "OPENCODE_CONFIG");
        assert!(env.contains(&("PWD".to_string(), a.cwd.clone())), "{env:?}");
        let cfg: Value =
            serde_json::from_str(&std::fs::read_to_string(&env[0].1).unwrap()).unwrap();
        assert_eq!(
            cfg["mcp"]["drovr"]["command"],
            json!(["/opt/drovr", "mcp", "--agent", "api"])
        );
        assert_eq!(
            cfg["mcp"]["drovr"]["environment"]["DROVR_HOME"],
            "/data/drovr"
        );
        assert!(cfg["instructions"][0].as_str().unwrap().ends_with(".md"));
        // OpenCode 2 loads a configured plugin from a folder.
        let plugin = PathBuf::from(cfg["plugins"][0].as_str().unwrap());
        assert_eq!(plugin, dir.join(PLUGIN_DIR));
        let package: Value =
            serde_json::from_str(&std::fs::read_to_string(plugin.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(package["type"], "module");
        let source = std::fs::read_to_string(plugin.join("server.js")).unwrap();
        assert!(source.contains(r#"const CLI = "/opt/drovr";"#), "{source}");
        assert!(source.contains(r#"spawn(CLI, ["hook", "permission", AGENT]"#));
        assert!(source.contains("export default {"));
        assert!(source.contains(r#"ctx.tool.hook("execute.before""#));
        assert!(!source.contains("Bun."), "runs under Node too");
        // An interactive session answers its own prompts: no plugin.
        let attach = dir.join(ATTACH_CONFIG);
        assert_eq!(
            cmd,
            format!(
                "OPENCODE_CONFIG={} opencode --standalone --session ses_1",
                attach.display()
            )
        );
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(attach).unwrap()).unwrap();
        assert!(cfg["plugins"].is_null());
        assert!(cfg["mcp"]["drovr"].is_object());
        assert!(cfg["model"].is_null(), "OpenCode's default");
    }

    #[test]
    fn the_tui_takes_its_model_from_the_config() {
        let mut cfg = Config::default();
        cfg.opencode.model = "gw/anthropic/claude-opus-5.5".into();
        cfg.opencode.agent = "build".into();
        let a = agent("ses_1");
        let (args, path) = crate::harness::testing::with_config(&a, Session::Resume, cfg, |r| {
            (OpenCode.herdr_args(r).unwrap(), attach_config(r).unwrap())
        });
        assert_eq!(args, ["--standalone", "--session", "ses_1"]);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(cfg["model"], "gw/anthropic/claude-opus-5.5");
        assert_eq!(cfg["default_agent"], "build");
    }

    #[test]
    fn reads_opencode_events() {
        let mut run = Value::Null;
        let mut parse = |s: &str| OpenCode.parse(&serde_json::from_str(s).unwrap(), &mut run);
        assert_eq!(
            parse(
                r#"{"type":"step_start","timestamp":1000,"sessionID":"ses_a","part":{"type":"step-start"}}"#
            ),
            Event::Session("ses_a".into())
        );
        let Event::Lines(l) = parse(
            r#"{"type":"tool_use","timestamp":1500,"sessionID":"ses_a","part":{"partID":"p1","type":"tool","id":"c1","tool":"shell","state":{"status":"completed","input":{"command":"ls","description":"List"},"output":"a.txt\n","title":"shell","metadata":{}}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].text.as_str(), l[1].kind.as_str()),
            ("shell ls", "result")
        );
        let Event::Lines(l) = parse(
            r#"{"type":"tool_use","timestamp":1600,"sessionID":"ses_a","part":{"type":"tool","tool":"edit","state":{"status":"error","input":{"path":"/r/a.rs"},"error":"Denied by you."}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].text.as_str(), l[1].kind.as_str()),
            ("edit /r/a.rs", "error")
        );
        // A step that calls tools doesn't end the run.
        assert_eq!(
            parse(
                r#"{"type":"step_finish","timestamp":2000,"sessionID":"ses_a","part":{"type":"step-finish","reason":"tool-calls","cost":0.25,"tokens":{"input":100,"output":20,"reasoning":5,"cache":{"read":1000,"write":50}}}}"#
            ),
            Event::Skip
        );
        let Event::Lines(l) = parse(
            r#"{"type":"text","timestamp":2500,"sessionID":"ses_a","part":{"type":"text","text":"Done.\n","time":{"start":1,"end":2}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(l[0].text, "Done.");
        let Event::End(end) = parse(
            r#"{"type":"step_finish","timestamp":4000,"sessionID":"ses_a","part":{"type":"step-finish","reason":"stop","cost":0.5,"tokens":{"input":10,"output":30,"reasoning":0,"cache":{"read":1200,"write":0}}}}"#,
        ) else {
            panic!("the last step ends the run")
        };
        assert_eq!(end["is_error"], false);
        assert_eq!(end["session_id"], "ses_a");
        assert_eq!(end["cost_usd"], 0.75);
        assert_eq!(end["num_turns"], 2);
        assert_eq!(end["duration_ms"], 3000.0);
        assert_eq!(
            end["tokens"],
            json!({ "input": 110, "output": 55, "cache_read": 2200, "cache_write": 50 })
        );
        assert_eq!(end["context_tokens"], 1210);
        assert_eq!(OpenCode.run_cost(&end, &mut Costs::default()), Some(0.75));
    }

    #[test]
    fn an_error_fails_the_run() {
        let mut run = Value::Null;
        let Event::Lines(l) = OpenCode.parse(
            &json!({"type":"error","sessionID":"s","error":{"type":"unknown","message":"Model unavailable"}}),
            &mut run,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].kind.as_str(), l[0].text.as_str()),
            ("error", "Model unavailable")
        );
        // OpenCode 1's shape.
        let Event::Lines(l) = OpenCode.parse(
            &json!({"type":"error","sessionID":"s","error":{"name":"APIError","data":{"message":"rate limited"}}}),
            &mut run,
        ) else {
            panic!()
        };
        assert_eq!(l[0].text, "rate limited");
        let Event::End(end) = OpenCode.parse(
            &json!({"type":"step_finish","sessionID":"s","part":{"reason":"stop","cost":0,"tokens":{}}}),
            &mut run,
        ) else {
            panic!()
        };
        assert_eq!(end["is_error"], true);
    }

    #[test]
    fn context_is_the_last_steps_input() {
        let log = [
            json!({"type":"step_finish","sessionID":"s","part":{"reason":"tool-calls","cost":0.1,"tokens":{"input":5,"cache":{"read":100,"write":0}}}}),
            json!({"type":"step_finish","sessionID":"s","part":{"reason":"stop","cost":0.1,"tokens":{"input":7,"cache":{"read":300,"write":20}}}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(OpenCode.context_tokens(&log), Some(327));
    }

    /// OpenCode 2's tables, with the columns drovr reads.
    fn test_db() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            r#"
            CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
              title TEXT, cost REAL DEFAULT 0 NOT NULL, time_created INTEGER NOT NULL,
              time_updated INTEGER NOT NULL, time_archived INTEGER);
            CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
              type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
              time_updated INTEGER NOT NULL, data TEXT NOT NULL);
            INSERT INTO session_v2 VALUES
              ('ses_a', NULL, '/r/api', 'Fix the login bug', 1.0, 1760000000000, 1760000500000, NULL),
              ('ses_sub', 'ses_a', '/r/api', 'a subagent', 0.25, 1760000100000, 1760000200000, NULL),
              ('ses_b', NULL, '/r/web', NULL, 0, 1760000300000, 1760000400000, NULL),
              ('ses_old', NULL, '/r/api', 'archived', 0, 1750000000000, 1750000000000, 1760000000000);
            INSERT INTO session_message VALUES
              ('m1', 'ses_a', 'user', 1, 0, 0, '{}'),
              ('m2', 'ses_a', 'assistant', 2, 0, 0,
               '{"model":{"id":"claude-opus-5.5","providerID":"gw"},"tokens":{"input":5,"output":1,"reasoning":0,"cache":{"read":100,"write":0}}}'),
              ('m3', 'ses_a', 'assistant', 3, 0, 0,
               '{"model":{"id":"claude-sonnet-5.5","providerID":"gw"},"tokens":{"input":12,"output":40,"reasoning":0,"cache":{"read":9000,"write":300}}}');
            "#,
        )
        .unwrap();
        db
    }

    #[test]
    fn reads_saved_sessions() {
        let db = test_db();
        // Newest first; no subagents' or archived sessions.
        let s = saved_sessions(&db, None, 10).unwrap();
        let ids: Vec<&str> = s.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["ses_a", "ses_b"]);
        assert_eq!(s[0].cwd, "/r/api");
        assert_eq!(s[0].first_prompt, "Fix the login bug");
        assert_eq!(s[0].modified, 1_760_000_500.0);
        assert_eq!(saved_sessions(&db, None, 1).unwrap().len(), 1);
        let b = saved_sessions(&db, Some("ses_b"), 1).unwrap();
        assert_eq!(
            (b[0].id.as_str(), b[0].first_prompt.as_str()),
            ("ses_b", "")
        );
        assert!(saved_sessions(&db, Some("ses_x"), 1).unwrap().is_empty());
        assert!(OpenCode.find_session("x'; DROP TABLE session; --").is_err());
    }

    #[test]
    fn finds_the_session_a_pane_started() {
        let db = test_db();
        let newest = |cwd: &str, since: f64| newest_session(&db, cwd, since).unwrap();
        assert_eq!(newest("/r/api", 1_760_000_000.0).as_deref(), Some("ses_a"));
        assert_eq!(newest("/r/api", 1_760_000_001.0), None);
        assert_eq!(newest("/r/web", 0.0).as_deref(), Some("ses_b"));
    }

    #[test]
    fn starts_in_a_herdr_pane() {
        assert!(OpenCode.runs_in_herdr());
        assert_eq!(OpenCode.herdr_kind(), "opencode");
        let a = agent("ses_1");
        let (resumed, env, dir) = with(&a, Session::Resume, |r| {
            (
                OpenCode.herdr_args(r).unwrap(),
                OpenCode.herdr_env(r).unwrap(),
                r.dir.clone(),
            )
        });
        assert_eq!(resumed, ["--standalone", "--session", "ses_1"]);
        assert_eq!(
            env,
            [(
                "OPENCODE_CONFIG".to_string(),
                dir.join(ATTACH_CONFIG).display().to_string()
            )]
        );
        // OpenCode 2's TUI can't fork.
        assert!(with(&a, Session::Fork, |r| OpenCode.herdr_args(r)).is_err());
        // A session drovr hasn't learned yet: a new one, not a guess.
        let none = with(&agent(""), Session::Resume, |r| {
            OpenCode.herdr_args(r).unwrap()
        });
        assert_eq!(none, ["--standalone"]);
    }

    #[test]
    fn reads_a_live_session() {
        let db = test_db();
        // Its subagents' costs count; the last answer gives context and model.
        assert_eq!(
            live_session(&db, "ses_a").unwrap(),
            Some(Live {
                session: "ses_a".into(),
                total_cost_usd: 1.25,
                context_tokens: 9312,
                model: "claude-sonnet-5.5".into(),
            })
        );
        // Before the first answer.
        let b = live_session(&db, "ses_b").unwrap().unwrap();
        assert_eq!((b.total_cost_usd, b.context_tokens), (0.0, 0));
        assert_eq!(live_session(&db, "ses_x").unwrap(), None);
        assert_eq!(OpenCode.session_live(&Config::default(), "x'; --"), None);
    }

    #[test]
    fn reads_a_sessions_transcript() {
        let row = |role: &str, t: i64, part: Value| json!({ "role": role, "t": t, "part": part.to_string() });
        let rows = [
            row(
                "user",
                1000,
                json!({ "type": "text", "text": "Fix the login bug" }),
            ),
            row(
                "user",
                1000,
                json!({ "type": "text", "text": "<file contents>", "synthetic": true }),
            ),
            row("assistant", 1100, json!({ "type": "step-start" })),
            row(
                "assistant",
                1200,
                json!({ "type": "tool", "tool": "bash", "state": { "status": "completed", "input": { "command": "ls" }, "output": "a.rs" } }),
            ),
            row(
                "assistant",
                1250,
                json!({ "type": "tool", "tool": "edit", "state": { "status": "running", "input": {} } }),
            ),
            row(
                "assistant",
                1300,
                json!({ "type": "step-finish", "reason": "tool-calls", "cost": 0.5, "tokens": { "input": 10, "output": 5, "cache": { "read": 0, "write": 0 } } }),
            ),
            row(
                "assistant",
                1400,
                json!({ "type": "text", "text": "Fixed.", "time": { "start": 1, "end": 2 } }),
            ),
            row(
                "assistant",
                1500,
                json!({ "type": "text", "text": "still typing", "time": { "start": 1 } }),
            ),
            row(
                "assistant",
                3000,
                json!({ "type": "step-finish", "reason": "stop", "cost": 0.25, "tokens": { "input": 20, "output": 5, "cache": { "read": 0, "write": 0 } } }),
            ),
            row(
                "user",
                5000,
                json!({ "type": "text", "text": "Now add a test" }),
            ),
        ];
        let events = transcript_events("ses_1", &rows);
        let mut run = Value::Null;
        let shown: Vec<String> = events
            .iter()
            .flat_map(|v| match OpenCode.parse_transcript(v, &mut run) {
                Event::Lines(l) => l
                    .into_iter()
                    .map(|l| format!("{} {}", l.kind, l.text))
                    .collect(),
                Event::End(end) => vec![format!("end {} {}", end["cost_usd"], end["duration_ms"])],
                _ => vec![],
            })
            .collect();
        assert_eq!(
            shown,
            [
                "prompt Fix the login bug",
                "tool bash ls",
                "result a.rs",
                "text Fixed.",
                // The turn's cost and time, from its prompt.
                "end 0.75 2000.0",
                "prompt Now add a test",
            ]
        );
    }

    #[test]
    fn asks_as_claude_code_would() {
        let servers: Vec<String> = ["my_gw", "my", "acme.docs", "drovr"]
            .map(String::from)
            .to_vec();
        for (tool, servers, want) in [
            ("shell", &servers[..], "Bash"),
            ("bash", &servers[..], "Bash"),
            ("patch", &servers[..], "Edit"),
            ("apply_patch", &servers[..], "Edit"),
            ("subagent", &servers[..], "Task"),
            ("execute", &servers[..], "CodeMode"),
            // OpenCode's own tools keep their names.
            ("worktree_create", &servers[..], "worktree_create"),
            ("drovr_send_message", &[][..], "mcp__drovr__send_message"),
            (
                "drovr_send_message",
                &servers[..],
                "mcp__drovr__send_message",
            ),
            // No servers known: split at the first `_`.
            (
                "atlassian_getJiraIssue",
                &[][..],
                "mcp__atlassian__getJiraIssue",
            ),
            ("my_gw_search", &[][..], "mcp__my__gw_search"),
            // The longest server name that fits wins.
            ("my_gw_search", &servers[..], "mcp__my_gw__search"),
            ("my_lookup_user", &servers[..], "mcp__my__lookup_user"),
            // Names are sanitized as OpenCode does.
            ("acme_docs_find", &servers[..], "mcp__acme_docs__find"),
            // A server's name alone isn't a tool of it.
            ("my_", &servers[..], "my_"),
        ] {
            assert_eq!(claude_name(tool, servers), want, "{tool}");
        }
        assert_eq!(claude_input(&json!({"path": "/r/a"}))["file_path"], "/r/a");
        assert_eq!(
            claude_input(&json!({"filePath": "/r/a"}))["file_path"],
            "/r/a"
        );

        let mut cfg = Config::default();
        let ask = |cfg: &Config, name: &str, input: Value| settled(cfg, "/r/api", name, &input);
        assert!(ask(&cfg, "Bash", json!({"command": "ls"})).is_none());
        assert!(ask(&cfg, "Task", json!({})).is_some());
        assert!(ask(&cfg, "CodeMode", json!({"code": "x"})).is_some());
        assert!(ask(&cfg, "worktree_create", json!({})).is_none());
        // acceptEdits (the default): edits inside the working directory.
        assert_eq!(
            ask(&cfg, "Edit", json!({"file_path": "/r/api/src/a.rs"})).unwrap()["behavior"],
            "allow"
        );
        assert_eq!(
            ask(&cfg, "Write", json!({"file_path": "src/new.rs"})).unwrap()["behavior"],
            "allow"
        );
        assert!(ask(&cfg, "Edit", json!({"file_path": "/r/api/../web/a.rs"})).is_none());
        assert!(ask(&cfg, "Edit", json!({"file_path": "/etc/hosts"})).is_none());
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** Add File: /tmp/x\n+x\n*** End Patch";
        assert!(ask(&cfg, "Edit", json!({"patchText": patch})).is_none());
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** End Patch";
        assert!(ask(&cfg, "Edit", json!({"patchText": patch})).is_some());
        // …and in the plans folder, but not next to it or out of it.
        cfg.reports.dir = "/home/me/.claude/plans".into();
        for (file, allowed) in [
            ("/home/me/.claude/plans/sre-3425/plan.html", true),
            ("/home/me/.claude/plans/notes.md", true),
            ("/home/me/.claude/plans-old/x.md", false),
            ("/home/me/.claude/plans/../settings.json", false),
            ("/home/me/.claude/CLAUDE.md", false),
        ] {
            assert_eq!(
                ask(&cfg, "Write", json!({"file_path": file})).is_some(),
                allowed,
                "{file}"
            );
        }
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** Add File: /home/me/.claude/plans/p.md\n+x\n*** End Patch";
        assert!(ask(&cfg, "Edit", json!({"patchText": patch})).is_some());
        // No plans folder: only the working directory.
        cfg.reports.dir = String::new();
        assert!(ask(&cfg, "Edit", json!({"file_path": "/etc/hosts"})).is_none());
        assert!(ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).is_some());
        cfg.reports.dir = "/home/me/.claude/plans".into();
        cfg.defaults.permission_mode = "auto".into();
        assert!(ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).is_some());
        assert!(ask(
            &cfg,
            "Edit",
            json!({"file_path": "/home/me/.claude/plans/a.md"})
        )
        .is_some());
        assert!(ask(&cfg, "Edit", json!({"file_path": "/etc/hosts"})).is_none());
        assert!(ask(&cfg, "Bash", json!({"command": "ls"})).is_none());
        cfg.defaults.permission_mode = "default".into();
        assert!(ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).is_none());
        assert!(ask(
            &cfg,
            "Edit",
            json!({"file_path": "/home/me/.claude/plans/a.md"})
        )
        .is_none());
        cfg.defaults.permission_mode = "plan".into();
        assert_eq!(
            ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).unwrap()["behavior"],
            "deny"
        );
        cfg.defaults.permission_mode = "bypassPermissions".into();
        assert!(ask(&cfg, "Bash", json!({"command": "rm -rf /"})).is_some());
    }
}
