//! OpenAI's Codex CLI (`codex exec`, experimental). It runs in Codex's own
//! sandbox with no approval prompts, so anything outside it goes through
//! `propose_command`. Headless only.

use super::{line, Event, Harness, Run, Session};
use crate::config::Config;
use crate::engine::truncate;
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub struct Codex;

impl Harness for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn label(&self) -> &'static str {
        "Codex"
    }

    fn command<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.codex.command
    }

    fn model<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.codex.model
    }

    /// Codex names the session when the first run starts.
    fn picks_session_id(&self) -> bool {
        false
    }

    fn headless_args(&self, run: &Run) -> Result<Vec<String>> {
        // Options go before `resume`, which takes the session and the prompt.
        let mut args: Vec<String> = ["exec", "--json", "--skip-git-repo-check"]
            .map(String::from)
            .to_vec();
        args.extend(codex_common(run)?);
        // No one answers approval prompts in a headless run.
        args.extend(["-c".into(), "approval_policy=\"never\"".into()]);
        match run.session {
            Session::New => {}
            Session::Resume => args.extend(["resume".into(), run.agent.session_id.clone()]),
            Session::Fork => bail!("Codex agents can't fork a session"),
        }
        // The prompt comes from stdin.
        args.push("-".into());
        Ok(args)
    }

    fn parse(&self, v: &Value) -> Event {
        codex_event(v)
    }

    fn attach_command(&self, run: &Run) -> Result<String> {
        use crate::shell::quote;
        let id = &run.agent.session_id;
        if id.is_empty() {
            bail!("{} hasn't started a Codex session yet", run.agent.name);
        }
        let mut args = vec![
            self.command(run.cfg).to_string(),
            "resume".into(),
            "--include-non-interactive".into(),
            id.clone(),
        ];
        args.extend(codex_common(run)?);
        Ok(args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "))
    }
}

/// A string as a TOML value for Codex's `-c key=value`. JSON's string
/// escapes are valid in TOML basic strings.
fn toml_str(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// The flags every Codex start shares: drovr's MCP server (whose tools can
/// wait for you for many minutes, so with a long timeout and no approval
/// prompt), the system prompt as developer instructions, the sandbox and
/// the model.
fn codex_common(run: &Run) -> Result<Vec<String>> {
    let cfg = run.cfg;
    let instructions = std::fs::read_to_string(&run.system_prompt)?;
    let args_toml = format!("[\"mcp\", \"--agent\", {}]", toml_str(&run.agent.name));
    let env_toml = format!(
        "{{ DROVR_HOME = {}, DROVR_AGENT = {} }}",
        toml_str(&run.home.display().to_string()),
        toml_str(&run.agent.name)
    );
    let mut args = vec![];
    for (key, value) in [
        (
            "mcp_servers.drovr.command",
            toml_str(&run.cli.display().to_string()),
        ),
        ("mcp_servers.drovr.args", args_toml),
        ("mcp_servers.drovr.env", env_toml),
        ("mcp_servers.drovr.tool_timeout_sec", "3600".into()),
        (
            "mcp_servers.drovr.default_tools_approval_mode",
            "\"approve\"".into(),
        ),
        ("developer_instructions", toml_str(&instructions)),
    ] {
        args.extend(["-c".into(), format!("{key}={value}")]);
    }
    args.extend(["-s".into(), cfg.codex.sandbox.clone()]);
    let model = super::model_of(&Codex, run.agent, cfg);
    if !model.is_empty() {
        args.extend(["-m".into(), model]);
    }
    args.extend(cfg.codex.extra_args.iter().cloned());
    Ok(args)
}

/// Codex's `exec --json`: `thread.started`, then `item.completed` events for
/// what it did, then `turn.completed` (with token counts) or `turn.failed`.
fn codex_event(v: &Value) -> Event {
    let end = |is_error: bool, usage: &Value| {
        Event::End(json!({
            "is_error": is_error,
            "session_id": null,
            "total_cost_usd": null,
            "duration_ms": null,
            "num_turns": null,
            "input_tokens": usage["input_tokens"],
            "output_tokens": usage["output_tokens"],
            "tokens": {
                "input": usage["input_tokens"],
                "output": usage["output_tokens"],
                "cache_read": usage["cached_input_tokens"],
                "cache_write": null,
            },
            "models": null,
        }))
    };
    match v["type"].as_str() {
        Some("thread.started") => match v["thread_id"].as_str() {
            Some(id) => Event::Session(id.to_string()),
            None => Event::Skip,
        },
        Some("turn.completed") => end(false, &v["usage"]),
        Some("turn.failed") => end(true, &Value::Null),
        // Retries ("Reconnecting... 2/5") are noise; a turn that gives up
        // ends with turn.failed, shown below.
        Some("error") => match v["message"].as_str() {
            Some(m) if !m.starts_with("Reconnecting") => {
                Event::Lines(vec![line("error", truncate(m, 600))])
            }
            _ => Event::Skip,
        },
        Some("item.completed") => {
            let item = &v["item"];
            let lines = match item["type"].as_str() {
                Some("agent_message") => {
                    vec![line("text", item["text"].as_str().unwrap_or("").into())]
                }
                Some("command_execution") => {
                    let failed = item["exit_code"].as_i64().is_some_and(|c| c != 0)
                        || item["status"] == "failed";
                    vec![
                        line(
                            "tool",
                            format!(
                                "shell {}",
                                truncate(item["command"].as_str().unwrap_or(""), 300)
                            ),
                        ),
                        line(
                            if failed { "error" } else { "result" },
                            truncate(item["aggregated_output"].as_str().unwrap_or(""), 600),
                        ),
                    ]
                }
                Some("file_change") => {
                    let paths: Vec<&str> = item["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["path"].as_str())
                        .collect();
                    vec![line("tool", format!("edit {}", paths.join(", ")))]
                }
                Some("mcp_tool_call") => {
                    let mut out = vec![line(
                        "tool",
                        format!(
                            "{}.{} {}",
                            item["server"].as_str().unwrap_or("mcp"),
                            item["tool"].as_str().unwrap_or("tool"),
                            truncate(&item["arguments"].to_string(), 200)
                        ),
                    )];
                    if item["status"] == "failed" {
                        out.push(line("error", truncate(&item["error"].to_string(), 600)));
                    }
                    out
                }
                Some("web_search") => vec![line(
                    "tool",
                    format!("web_search {}", item["query"].as_str().unwrap_or("")),
                )],
                Some("error") => vec![line(
                    "error",
                    truncate(item["message"].as_str().unwrap_or(""), 600),
                )],
                _ => vec![],
            };
            Event::Lines(lines)
        }
        _ => Event::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{agent, with};

    #[test]
    fn codex_args() {
        let a = agent("");
        let new = with(&a, Session::New, |r| Codex.headless_args(r).unwrap());
        assert_eq!(new[..3], ["exec", "--json", "--skip-git-repo-check"]);
        assert_eq!(new.last().unwrap(), "-");
        assert!(!new.contains(&"resume".to_string()));
        let joined = new.join(" ");
        for part in [
            "mcp_servers.drovr.command=\"/opt/drovr\"",
            "mcp_servers.drovr.args=[\"mcp\", \"--agent\", \"api\"]",
            "mcp_servers.drovr.env={ DROVR_HOME = \"/data/drovr\", DROVR_AGENT = \"api\" }",
            "developer_instructions=\"You are \\\"api\\\".\\nLine two.\"",
            "approval_policy=\"never\"",
            "-s workspace-write",
        ] {
            assert!(joined.contains(part), "{part} in {joined}");
        }
        let a = agent("t-1");
        let resumed = with(&a, Session::Resume, |r| Codex.headless_args(r).unwrap());
        assert_eq!(resumed[resumed.len() - 3..], ["resume", "t-1", "-"]);
        assert!(with(&a, Session::Fork, |r| Codex.headless_args(r)).is_err());
        assert!(with(&agent(""), Session::Resume, |r| { Codex.attach_command(r) }).is_err());
    }

    #[test]
    fn reads_codex_events() {
        let parse = |s: &str| Codex.parse(&serde_json::from_str(s).unwrap());
        // Real `codex exec --json` output (0.164.0).
        assert_eq!(
            parse(
                r#"{"type":"thread.started","thread_id":"01a17aed-a0eb-7763-ba4a-16b8c2b773b8"}"#
            ),
            Event::Session("01a17aed-a0eb-7763-ba4a-16b8c2b773b8".into())
        );
        assert_eq!(
            parse(r#"{"type":"error","message":"Reconnecting... 2/5 (unexpected status 401)"}"#),
            Event::Skip
        );
        let Event::End(failed) = parse(r#"{"type":"turn.failed","error":{"message":"401"}}"#)
        else {
            panic!("turn.failed ends the run")
        };
        assert_eq!(failed["is_error"], true);
        let Event::End(done) = parse(
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":0,"output_tokens":80}}"#,
        ) else {
            panic!("turn.completed ends the run")
        };
        assert_eq!(
            (done["is_error"].clone(), done["input_tokens"].clone()),
            (json!(false), json!(1200))
        );
        let Event::Lines(l) = parse(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"a.txt\n","exit_code":0,"status":"completed"}}"#,
        ) else {
            panic!()
        };
        assert_eq!(
            (l[0].text.as_str(), l[1].kind.as_str()),
            ("shell bash -lc ls", "result")
        );
    }
}
