//! Claude Code (`claude`): drovr's permission prompt, cost tracking, herdr
//! panes, forking and adopting existing sessions.

use super::{line, Costs, Event, Harness, Run, Session};
use crate::config::Config;
use crate::engine::truncate;
use crate::sessions::SavedSession;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Claude;

impl Harness for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn command<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.claude.command
    }

    fn model<'a>(&self, cfg: &'a Config) -> &'a str {
        &cfg.defaults.model
    }

    fn picks_session_id(&self) -> bool {
        true
    }

    fn headless_args(&self, run: &Run) -> Result<Vec<String>> {
        let cfg = run.cfg;
        let id = run.agent.session_id.clone();
        let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
            .map(String::from)
            .to_vec();
        match run.session {
            Session::New => args.extend(["--session-id".into(), id]),
            Session::Resume => args.extend(["--resume".into(), id]),
            Session::Fork => args.extend(["--resume".into(), id, "--fork-session".into()]),
        }
        args.extend([
            "--permission-prompt-tool".into(),
            "mcp__drovr__approve".into(),
        ]);
        if cfg.defaults.max_budget_usd > 0.0 {
            args.extend([
                "--max-budget-usd".into(),
                cfg.defaults.max_budget_usd.to_string(),
            ]);
        }
        args.extend(claude_common(run));
        Ok(args)
    }

    fn runs_in_herdr(&self) -> bool {
        true
    }

    fn herdr_args(&self, run: &Run) -> Result<Vec<String>> {
        let id = run.agent.session_id.clone();
        let mut args = match run.session {
            Session::New => vec!["--session-id".to_string(), id],
            _ => vec!["--resume".to_string(), id],
        };
        args.extend(["-n".into(), run.agent.name.clone()]);
        if run.session == Session::Fork {
            args.push("--fork-session".into());
        }
        args.extend(claude_common(run));
        Ok(args)
    }

    fn parse(&self, v: &Value) -> Event {
        claude_event(v)
    }

    fn run_cost(&self, end: &Value, costs: &mut Costs) -> Option<f64> {
        let total = end["total_cost_usd"].as_f64()?;
        Some(costs.added(end["session_id"].as_str().unwrap_or(""), total))
    }

    /// The last assistant message's input: fresh, read from the cache and
    /// written to it, which is everything the model was given.
    fn context_tokens(&self, log: &str) -> Option<u64> {
        log.lines().rev().find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            if v["type"] != "assistant" {
                return None;
            }
            let u = &v["message"]["usage"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            u.is_object().then(|| {
                n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens")
            })
        })
    }

    fn session_file(&self, id: &str) -> Option<PathBuf> {
        crate::sessions::file(id)
    }

    fn find_session(&self, id: &str) -> Result<SavedSession> {
        crate::sessions::find(id)
            .ok_or_else(|| anyhow!("no Claude Code session {id} under ~/.claude/projects"))
    }

    fn recent_sessions(&self, limit: usize) -> Vec<SavedSession> {
        crate::sessions::recent(limit)
    }

    fn attach_command(&self, run: &Run) -> Result<String> {
        use crate::shell::quote;
        let mut args = vec![
            self.command(run.cfg).to_string(),
            "--resume".into(),
            run.agent.session_id.clone(),
        ];
        args.extend(claude_common(run));
        Ok(args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "))
    }
}

/// The flags every Claude Code start shares: drovr's MCP server and system
/// prompt, the permission mode, model and tool limits.
fn claude_common(run: &Run) -> Vec<String> {
    let cfg = run.cfg;
    let mut args = vec![
        "--mcp-config".to_string(),
        run.mcp_config.display().to_string(),
        "--append-system-prompt-file".into(),
        run.system_prompt.display().to_string(),
        "--permission-mode".into(),
        cfg.defaults.permission_mode.clone(),
    ];
    let model = super::model_of(&Claude, run.agent, cfg);
    if !model.is_empty() {
        args.extend(["--model".into(), model]);
    }
    if !cfg.defaults.disallowed_tools.is_empty() {
        args.push("--disallowedTools".into());
        args.push(cfg.defaults.disallowed_tools.join(","));
    }
    args.extend(cfg.claude.extra_args.iter().cloned());
    args
}

/// Claude Code's `--output-format stream-json`.
fn claude_event(v: &Value) -> Event {
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => match v["session_id"].as_str() {
            Some(id) => Event::Session(id.to_string()),
            None => Event::Skip,
        },
        Some("assistant") => {
            let mut out = vec![];
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => out.push(line(
                        "text",
                        block["text"].as_str().unwrap_or("").to_string(),
                    )),
                    Some("tool_use") => {
                        let input = &block["input"];
                        let summary = [
                            "command",
                            "file_path",
                            "pattern",
                            "url",
                            "query",
                            "to",
                            "question",
                        ]
                        .iter()
                        .find_map(|k| input[*k].as_str().map(String::from))
                        .unwrap_or_else(|| truncate(&input.to_string(), 200));
                        out.push(line(
                            "tool",
                            format!(
                                "{} {}",
                                block["name"].as_str().unwrap_or("tool"),
                                truncate(&summary, 300)
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            Event::Lines(out)
        }
        Some("user") => {
            let mut out = vec![];
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] == "tool_result" {
                    let text = match &block["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        other => other.to_string(),
                    };
                    let kind = if block["is_error"] == true {
                        "error"
                    } else {
                        "result"
                    };
                    out.push(line(kind, truncate(&text, 600)));
                }
            }
            Event::Lines(out)
        }
        Some("result") => Event::End(json!({
            "is_error": v["is_error"] == true,
            "session_id": v["session_id"],
            "total_cost_usd": v["total_cost_usd"],
            "duration_ms": v["duration_ms"],
            "num_turns": v["num_turns"],
            "input_tokens": null,
            "output_tokens": null,
            "tokens": {
                "input": v["usage"]["input_tokens"],
                "output": v["usage"]["output_tokens"],
                "cache_read": v["usage"]["cache_read_input_tokens"],
                "cache_write": v["usage"]["cache_creation_input_tokens"],
            },
            "models": v["modelUsage"].as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
        })),
        _ => Event::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{agent, with};

    #[test]
    fn claude_args_match_what_drovr_always_ran() {
        let a = agent("s1");
        let args = with(&a, Session::Fork, |r| Claude.headless_args(r).unwrap());
        assert_eq!(
            args[..10],
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--resume",
                "s1",
                "--fork-session",
                "--permission-prompt-tool",
                "mcp__drovr__approve",
                "--mcp-config"
            ]
        );
        let cmd = with(&a, Session::Resume, |r| Claude.attach_command(r).unwrap());
        assert!(cmd.starts_with("claude --resume s1 --mcp-config"), "{cmd}");
    }

    #[test]
    fn ends_carry_tokens_and_models() {
        let v: Value = serde_json::from_str(
            r#"{"type":"result","is_error":false,"session_id":"s1","total_cost_usd":0.5,
                "usage":{"input_tokens":12,"output_tokens":340,"cache_read_input_tokens":9000,"cache_creation_input_tokens":800},
                "modelUsage":{"claude-sonnet-5-5":{"costUSD":0.5}}}"#,
        )
        .unwrap();
        let Event::End(end) = claude_event(&v) else {
            panic!("not an end")
        };
        assert_eq!(
            end["tokens"],
            json!({ "input": 12, "output": 340, "cache_read": 9000, "cache_write": 800 })
        );
        assert_eq!(end["models"], json!(["claude-sonnet-5-5"]));
        // The run's last line still shows the cost, not tokens.
        assert!(end["input_tokens"].is_null());
    }

    #[test]
    fn reads_claude_events() {
        let v: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        )
        .unwrap();
        let Event::Lines(l) = Claude.parse(&v) else {
            panic!()
        };
        assert_eq!((l[0].text.as_str(), l[1].text.as_str()), ("hi", "Bash ls"));
        assert_eq!(
            Claude.parse(&json!({"type":"system","subtype":"init","session_id":"s9"})),
            Event::Session("s9".into())
        );
    }
}
