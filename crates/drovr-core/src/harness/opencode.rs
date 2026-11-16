//! OpenCode (`opencode`). Each prompt is an `opencode run --format json`
//! run. drovr's MCP server and system prompt reach it through a config file
//! drovr writes for the agent (`OPENCODE_CONFIG`), which OpenCode merges over
//! your own, and drovr's permission prompt through a plugin in that config:
//! before each tool call it asks `drovr hook permission`, which answers the
//! same way Claude Code's permission prompt is answered (`permissions.*`,
//! then the Inbox). Agents can also run in herdr panes (herdr's `opencode`
//! kind), where you answer OpenCode's own prompts; sessions can be adopted
//! and forked.

use super::{line, Costs, Event, Harness, Live, Run, Session};
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

/// The config a headless run reads; the plugin it loads.
const RUN_CONFIG: &str = "opencode-run.json";
const PLUGIN: &str = "opencode-permissions.js";
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
        let mut args: Vec<String> = ["run", "--format", "json", "--auto"]
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
        std::fs::create_dir_all(&run.dir)?;
        let plugin = run.dir.join(PLUGIN);
        std::fs::write(&plugin, plugin_source(run))?;
        let mut cfg = config(run);
        cfg["plugin"] = json!([format!("file://{}", plugin.display())]);
        let path = run.dir.join(RUN_CONFIG);
        std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
        Ok(vec![
            ("OPENCODE_CONFIG".into(), path.display().to_string()),
            // `--pure` (or this) would skip the plugin, and with it every
            // permission check.
            ("OPENCODE_PURE".into(), String::new()),
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
        sessions(&format!("id = '{id}'"), 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no OpenCode session {id} (see `opencode session list`)"))
    }

    fn recent_sessions(&self, limit: usize) -> Vec<SavedSession> {
        sessions("1 = 1", limit).unwrap_or_default()
    }

    fn runs_in_herdr(&self) -> bool {
        true
    }

    /// OpenCode's TUI. Until drovr knows the session a pane started (see
    /// `latest_session`), resuming starts a new one rather than guessing.
    fn herdr_args(&self, run: &Run) -> Result<Vec<String>> {
        let id = &run.agent.session_id;
        let mut args = match run.session {
            _ if id.is_empty() => vec![],
            Session::New => vec![],
            Session::Resume => vec!["--session".into(), id.clone()],
            Session::Fork => vec!["--session".into(), id.clone(), "--fork".into()],
        };
        args.extend(opencode_common(run));
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
        let sql = format!(
            "SELECT \
               (SELECT COALESCE(SUM(json_extract(m.data, '$.cost')), 0) FROM message m \
                 WHERE m.session_id IN (SELECT id FROM session WHERE id = '{id}' OR parent_id = '{id}')) AS cost, \
               (SELECT m.data FROM message m WHERE m.session_id = '{id}' \
                 AND json_extract(m.data, '$.role') = 'assistant' \
                 ORDER BY m.time_created DESC LIMIT 1) AS last"
        );
        let live = db(cfg, &sql)
            .ok()
            .and_then(|rows| live_of(id, rows.first()?));
        if let Ok(mut s) = seen.lock() {
            s.insert(id.to_string(), (Instant::now(), live.clone()));
        }
        live
    }

    fn latest_session(&self, cfg: &Config, cwd: &str, since: f64) -> Option<String> {
        let sql = format!(
            "SELECT id FROM session WHERE parent_id IS NULL AND directory = {} \
             AND time_created >= {} ORDER BY time_created DESC LIMIT 1",
            sql_str(cwd),
            (since * 1000.0) as i64
        );
        db(cfg, &sql).ok()?.first()?["id"]
            .as_str()
            .map(String::from)
    }

    fn attach_command(&self, run: &Run) -> Result<String> {
        let id = &run.agent.session_id;
        if id.is_empty() {
            bail!("{} hasn't started an OpenCode session yet", run.agent.name);
        }
        let path = attach_config(run)?;
        let mut args = vec![
            self.command(run.cfg).to_string(),
            "--session".into(),
            id.clone(),
        ];
        args.extend(opencode_common(run));
        Ok(format!(
            "OPENCODE_CONFIG={} {}",
            quote(&path.display().to_string()),
            args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        ))
    }
}

/// The flags every OpenCode start shares: the model, the OpenCode agent
/// and your extra arguments.
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
/// for you for many minutes, so with a long timeout) and the system prompt
/// as an instructions file.
fn config(run: &Run) -> Value {
    json!({
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
    })
}

/// The plugin that asks drovr before each tool call. A denial throws, which
/// OpenCode hands the model as the tool's error.
fn plugin_source(run: &Run) -> String {
    let js = |s: &str| Value::String(s.to_string()).to_string();
    format!(
        r#"// Written by drovr for agent {name}: asks drovr before each tool call,
// as Claude Code's permission prompt does. Rewritten before every run.
const CLI = {cli};
const AGENT = {agent};
const HOME = {home};

export const DrovrPermissions = async () => ({{
  "tool.execute.before": async (input, output) => {{
    const proc = Bun.spawn([CLI, "hook", "permission", AGENT], {{
      stdin: new Blob([JSON.stringify({{ tool: input.tool, args: output.args ?? {{}} }})]),
      stdout: "pipe",
      stderr: "pipe",
      env: {{ ...process.env, DROVR_HOME: HOME, DROVR_AGENT: AGENT }},
    }});
    const [out, err] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ]);
    await proc.exited;
    let answer;
    try {{
      answer = JSON.parse(out);
    }} catch {{
      throw new Error(`drovr couldn't check this tool call: ${{(err || out).trim()}}`);
    }}
    if (answer.behavior !== "allow") throw new Error(answer.message || "Denied.");
  }},
}});
"#,
        name = run.agent.name,
        cli = js(&run.cli.display().to_string()),
        agent = js(&run.agent.name),
        home = js(&run.home.display().to_string()),
    )
}

/// A session's figures and when they were read.
type Seen = (Instant, Option<Live>);

/// The config an interactive session reads, written fresh.
fn attach_config(run: &Run) -> Result<PathBuf> {
    std::fs::create_dir_all(&run.dir)?;
    let path = run.dir.join(ATTACH_CONFIG);
    std::fs::write(&path, serde_json::to_string_pretty(&config(run))?)?;
    Ok(path)
}

/// OpenCode's session IDs (`ses_…`): safe to put in a query as they are.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A string as an SQL literal.
fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Rows from OpenCode's database (`opencode db … --format json`, which
/// prints nothing when there are none).
fn db(cfg: &Config, sql: &str) -> Result<Vec<Value>> {
    let out = Command::new(&cfg.opencode.command)
        .args(["db", sql, "--format", "json"])
        .output()
        .with_context(|| format!("running {}", cfg.opencode.command))?;
    if !out.status.success() {
        bail!(
            "opencode db failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or_default())
}

/// A session's figures from `session_live`'s row: its cost, and the last
/// answer's message (`data`, as JSON text) for the context and model.
fn live_of(id: &str, row: &Value) -> Option<Live> {
    let last: Value = row["last"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let t = &last["tokens"];
    let n = |x: &Value| x.as_u64().unwrap_or(0);
    Some(Live {
        session: id.to_string(),
        total_cost_usd: row["cost"].as_f64()?,
        context_tokens: n(&t["input"]) + n(&t["cache"]["read"]) + n(&t["cache"]["write"]),
        model: last["modelID"].as_str().unwrap_or("").to_string(),
    })
}

/// Saved sessions from OpenCode's database, newest first. Subagents'
/// sessions (with a parent) and archived ones are left out.
fn sessions(filter: &str, limit: usize) -> Result<Vec<SavedSession>> {
    let sql = format!(
        "SELECT id, title, directory, time_updated FROM session \
         WHERE parent_id IS NULL AND time_archived IS NULL AND {filter} \
         ORDER BY time_updated DESC LIMIT {limit}"
    );
    Ok(parse_sessions(&db(
        &Config::load().unwrap_or_default(),
        &sql,
    )?))
}

/// Database rows as saved sessions.
fn parse_sessions(rows: &[Value]) -> Vec<SavedSession> {
    rows.iter()
        .filter_map(|r| {
            Some(SavedSession {
                id: r["id"].as_str()?.to_string(),
                cwd: r["directory"].as_str().unwrap_or("").to_string(),
                first_prompt: truncate(r["title"].as_str().unwrap_or(""), 200),
                modified: r["time_updated"].as_f64().unwrap_or(0.0) / 1000.0,
            })
        })
        .collect()
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
            let e = &v["error"];
            let text = e["data"]["message"]
                .as_str()
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
    let name = claude_name(tool);
    let input = claude_input(&req["args"]);
    let cfg = &engine.cfg;
    if cfg.defaults.disallowed_tools.contains(&name) {
        return Ok(
            json!({ "behavior": "deny", "message": format!("{name} isn't allowed for drovr agents.") }),
        );
    }
    let agent = engine.agent(me)?;
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
    // Never asked about in Claude Code: subagents, to-do lists, skills.
    if matches!(name, "Task" | "TodoRead" | "TodoWrite" | "Skill") {
        return allow;
    }
    let edit = matches!(name, "Edit" | "Write" | "MultiEdit");
    match cfg.defaults.permission_mode.as_str() {
        "bypassPermissions" => allow,
        "plan" if edit => Some(json!({
            "behavior": "deny",
            "message": "This agent runs in plan mode: it can't change files.",
        })),
        "acceptEdits" if edit && edits_inside(cwd, input) => allow,
        _ => None,
    }
}

/// Whether every file an edit touches is inside the working directory, as
/// Claude Code's `acceptEdits` requires.
fn edits_inside(cwd: &str, input: &Value) -> bool {
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
            p.starts_with(normalize(here))
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

/// OpenCode's tool as Claude Code names it. MCP tools are
/// `<server>_<tool>` in OpenCode; drovr's own are told apart exactly, and
/// any other is split at its first `_`.
pub fn claude_name(tool: &str) -> String {
    let builtin = match tool {
        "bash" => "Bash",
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "multiedit" => "MultiEdit",
        "apply_patch" | "patch" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "list" => "LS",
        "todowrite" => "TodoWrite",
        "todoread" => "TodoRead",
        "webfetch" => "WebFetch",
        "websearch" => "WebSearch",
        "task" => "Task",
        "skill" => "Skill",
        _ => "",
    };
    if !builtin.is_empty() {
        return builtin.into();
    }
    if let Some(rest) = tool.strip_prefix("drovr_") {
        return format!("mcp__drovr__{rest}");
    }
    match tool.split_once('_') {
        Some((server, name)) => format!("mcp__{server}__{name}"),
        None => tool.to_string(),
    }
}

/// OpenCode's arguments with Claude Code's names for the ones drovr reads
/// (`filePath` is `file_path`).
fn claude_input(args: &Value) -> Value {
    let mut input = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    if let Some(path) = input.get("filePath").cloned() {
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
        assert_eq!(new, ["run", "--format", "json", "--auto"]);
        let a = agent("ses_1");
        let resumed = with(&a, Session::Resume, |r| OpenCode.headless_args(r).unwrap());
        assert_eq!(resumed[4..], ["--session", "ses_1"]);
        let forked = with(&a, Session::Fork, |r| OpenCode.headless_args(r).unwrap());
        assert_eq!(forked[4..], ["--session", "ses_1", "--fork"]);
        let mut cfg = Config::default();
        cfg.opencode.model = "anthropic/claude-sonnet-5-5".into();
        cfg.opencode.agent = "build".into();
        let args = crate::harness::testing::with_config(&a, Session::Resume, cfg, |r| {
            OpenCode.headless_args(r).unwrap()
        });
        assert_eq!(
            args[6..],
            ["-m", "anthropic/claude-sonnet-5-5", "--agent", "build"]
        );
        assert!(with(&agent(""), Session::Resume, |r| OpenCode.attach_command(r)).is_err());
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
        let plugin = cfg["plugin"][0].as_str().unwrap();
        let source = std::fs::read_to_string(plugin.strip_prefix("file://").unwrap()).unwrap();
        assert!(source.contains(r#"const CLI = "/opt/drovr";"#), "{source}");
        assert!(source.contains(r#""hook", "permission", AGENT"#));
        // An interactive session answers its own prompts: no plugin.
        let attach = dir.join(ATTACH_CONFIG);
        assert!(
            cmd.starts_with(&format!(
                "OPENCODE_CONFIG={} opencode --session ses_1",
                attach.display()
            )),
            "{cmd}"
        );
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(attach).unwrap()).unwrap();
        assert!(cfg["plugin"].is_null());
        assert!(cfg["mcp"]["drovr"].is_object());
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
            r#"{"type":"tool_use","timestamp":1500,"sessionID":"ses_a","part":{"type":"tool","tool":"bash","callID":"c1","state":{"status":"completed","input":{"command":"ls","description":"List"},"output":"a.txt\n","title":"ls","metadata":{}}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].text.as_str(), l[1].kind.as_str()),
            ("bash ls", "result")
        );
        let Event::Lines(l) = parse(
            r#"{"type":"tool_use","timestamp":1600,"sessionID":"ses_a","part":{"type":"tool","tool":"edit","state":{"status":"error","input":{"filePath":"/r/a.rs"},"error":"Denied by you."}}}"#,
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
            &json!({"type":"error","sessionID":"s","error":{"name":"APIError","data":{"message":"rate limited"}}}),
            &mut run,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].kind.as_str(), l[0].text.as_str()),
            ("error", "rate limited")
        );
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

    #[test]
    fn reads_saved_sessions() {
        let out = r#"[{"id":"ses_b","title":"Fix the login bug","directory":"/r/api","time_updated":1760000000000}]"#;
        let s = parse_sessions(&serde_json::from_str::<Vec<Value>>(out).unwrap());
        assert_eq!((s[0].id.as_str(), s[0].cwd.as_str()), ("ses_b", "/r/api"));
        assert_eq!(s[0].first_prompt, "Fix the login bug");
        assert_eq!(s[0].modified, 1_760_000_000.0);
        assert!(parse_sessions(&[]).is_empty());
        assert!(OpenCode.find_session("x'; DROP TABLE session; --").is_err());
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
        assert_eq!(resumed, ["--session", "ses_1"]);
        assert_eq!(
            env,
            [(
                "OPENCODE_CONFIG".to_string(),
                dir.join(ATTACH_CONFIG).display().to_string()
            )]
        );
        let forked = with(&a, Session::Fork, |r| OpenCode.herdr_args(r).unwrap());
        assert_eq!(forked, ["--session", "ses_1", "--fork"]);
        // A session drovr hasn't learned yet: a new one, not a guess.
        let none = with(&agent(""), Session::Resume, |r| {
            OpenCode.herdr_args(r).unwrap()
        });
        assert!(none.is_empty());
    }

    #[test]
    fn reads_a_live_session() {
        let last = json!({ "role": "assistant", "modelID": "claude-sonnet-5-5",
            "tokens": { "input": 12, "output": 40, "cache": { "read": 9000, "write": 300 } } });
        let row = json!({ "cost": 1.25, "last": last.to_string() });
        assert_eq!(
            live_of("ses_1", &row),
            Some(Live {
                session: "ses_1".into(),
                total_cost_usd: 1.25,
                context_tokens: 9312,
                model: "claude-sonnet-5-5".into(),
            })
        );
        // Before the first answer.
        let row = json!({ "cost": 0, "last": null });
        assert_eq!(live_of("ses_1", &row).unwrap().context_tokens, 0);
        assert_eq!(sql_str("/r/it's"), "'/r/it''s'");
        assert_eq!(OpenCode.session_live(&Config::default(), "x'; --"), None);
    }

    #[test]
    fn asks_as_claude_code_would() {
        assert_eq!(claude_name("bash"), "Bash");
        assert_eq!(claude_name("apply_patch"), "Edit");
        assert_eq!(
            claude_name("drovr_send_message"),
            "mcp__drovr__send_message"
        );
        assert_eq!(
            claude_name("atlassian_getJiraIssue"),
            "mcp__atlassian__getJiraIssue"
        );
        assert_eq!(
            claude_input(&json!({"filePath": "/r/a"}))["file_path"],
            "/r/a"
        );

        let mut cfg = Config::default();
        let ask = |cfg: &Config, name: &str, input: Value| settled(cfg, "/r/api", name, &input);
        assert!(ask(&cfg, "Bash", json!({"command": "ls"})).is_none());
        assert!(ask(&cfg, "Task", json!({})).is_some());
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
        cfg.defaults.permission_mode = "default".into();
        assert!(ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).is_none());
        cfg.defaults.permission_mode = "plan".into();
        assert_eq!(
            ask(&cfg, "Edit", json!({"file_path": "/r/api/a.rs"})).unwrap()["behavior"],
            "deny"
        );
        cfg.defaults.permission_mode = "bypassPermissions".into();
        assert!(ask(&cfg, "Bash", json!({"command": "rm -rf /"})).is_some());
    }
}
