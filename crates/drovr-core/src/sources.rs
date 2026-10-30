//! Jira, Confluence and Slack lookups.
//!
//! Each source has a provider:
//! - `agent`: a one-shot `claude -p` that uses the Jira/Confluence/Slack MCP
//!   servers your Claude Code already has, and answers in JSON.
//! - `rest`: the Atlassian Cloud / Slack Web API with a token from the env.
//! - `command`: any CLI (twg, jira-cli, a script) that prints JSON.

use crate::config::{Config, Source};
use crate::engine::find_key;
use crate::shell::{self, extract_json};
use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Ticket {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub assignee: String,
    pub priority: String,
    pub issue_type: String,
    pub url: String,
    pub description: String,
    pub comments: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub space: String,
    pub url: String,
    pub excerpt: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackMessage {
    pub channel: String,
    pub user: String,
    pub ts: String,
    pub text: String,
    pub permalink: String,
}

const TICKET_SHAPE: &str = r#"{"key":"", "summary":"", "status":"", "assignee":"", "priority":"", "issue_type":"", "url":"", "description":"(plain text)", "comments":["author: text"]}"#;
const SEARCH_SHAPE: &str =
    r#"{"key":"", "summary":"", "status":"", "assignee":"", "priority":"", "issue_type":""}"#;
const PAGE_SHAPE: &str = r#"{"id":"", "title":"", "space":"", "url":"", "excerpt":"", "body":"(plain text or markdown)"}"#;
const SLACK_SHAPE: &str = r#"{"channel":"", "user":"", "ts":"", "text":"", "permalink":""}"#;

pub struct Sources<'a> {
    pub cfg: &'a Config,
    /// Told what a lookup is doing ("Calling searchJiraIssuesUsingJql") as it
    /// happens, for agent-provider lookups.
    pub progress: Option<&'a (dyn Fn(&str) + Sync)>,
}

/// Process groups of lookups in flight, so they can be cancelled.
static RUNNING: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

/// Stop every lookup in flight. Returns how many were running.
pub fn cancel_lookups() -> usize {
    let pids = RUNNING
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    for pid in &pids {
        unsafe { libc::killpg(*pid, libc::SIGTERM) };
    }
    pids.len()
}

/// A short, readable description of one stream-json event, if it's worth showing.
fn describe(v: &Value) -> Option<String> {
    match v["type"].as_str()? {
        "system" if v["subtype"] == "init" => Some("Connecting to your MCP servers…".into()),
        "assistant" => v["message"]["content"].as_array()?.iter().find_map(|b| {
            if b["type"] != "tool_use" {
                return None;
            }
            let name = b["name"].as_str()?;
            let short = name.rsplit("__").next().unwrap_or(name);
            Some(match short {
                "ToolSearch" => "Loading tools…".to_string(),
                _ => format!("Calling {short}"),
            })
        }),
        "user" => Some("Reading results…".into()),
        _ => None,
    }
}

impl<'a> Sources<'a> {
    pub fn new(cfg: &'a Config) -> Sources<'a> {
        Sources {
            cfg,
            progress: None,
        }
    }

    pub fn with_progress(cfg: &'a Config, progress: &'a (dyn Fn(&str) + Sync)) -> Sources<'a> {
        Sources {
            cfg,
            progress: Some(progress),
        }
    }

    fn say(&self, text: &str) {
        if let Some(f) = self.progress {
            f(text);
        }
    }

    // -- jira -----------------------------------------------------------------

    pub fn jira_search(&self, query: &str) -> Result<Vec<Ticket>> {
        let src = &self.cfg.jira;
        let jql = if query.trim().is_empty() {
            src.default_query.clone()
        } else {
            as_jql(query)
        };
        match src.provider.as_str() {
            "rest" => {
                let url = format!("{}/rest/api/3/search/jql", base(src)?);
                let body = json!({ "jql": jql, "maxResults": 50, "fields": ["summary", "status", "assignee", "priority", "issuetype"] });
                let v = rest_post(src, &url, &body)?;
                Ok(v["issues"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| jira_ticket(src, i))
                    .collect())
            }
            "command" => Ok(issue_list(command(&src.search_cmd, &[("query", &jql)])?)
                .iter()
                .map(|i| jira_ticket(src, i))
                .collect()),
            _ => {
                let v = self.ask_agent(&format!(
                    "Use your Jira MCP tools to search Jira with this JQL: {jql}\nReturn at most 20 issues as a JSON array of objects with only these fields: {SEARCH_SHAPE}."
                ))?;
                Ok(serde_json::from_value(v).unwrap_or_default())
            }
        }
    }

    /// Issues matching `jql` as Jira or the CLI returned them, for the
    /// watcher. Only the `command` and `rest` providers: asking an agent on
    /// every poll would spend tokens.
    pub fn jira_issues_raw(&self, jql: &str) -> Result<Vec<Value>> {
        let src = &self.cfg.jira;
        match src.provider.as_str() {
            "rest" => {
                let url = format!("{}/rest/api/3/search/jql", base(src)?);
                let body = json!({ "jql": jql, "maxResults": 100, "fields": ["summary", "status", "assignee", "priority", "issuetype", "updated"] });
                Ok(issue_list(rest_post(src, &url, &body)?))
            }
            "command" => Ok(issue_list(quiet_command(&src.search_cmd, &[("query", jql)])?)),
            p => bail!("the watcher reads Jira with the command or rest provider (jira.provider is {p:?}); asking an agent on every poll would spend tokens"),
        }
    }

    /// One issue with its comments, as Jira or the CLI returned it.
    pub fn jira_issue_raw(&self, key: &str) -> Result<Value> {
        let src = &self.cfg.jira;
        match src.provider.as_str() {
            "rest" => {
                let url = format!(
                    "{}/rest/api/3/issue/{}?fields=summary,status,assignee,priority,issuetype,updated,comment",
                    base(src)?,
                    urlencode(key)
                );
                rest_get(src, &url)
            }
            "command" => issue_list(quiet_command(&src.get_cmd, &[("key", key)])?)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("{key}: the Jira command returned no issue")),
            p => bail!("the watcher reads Jira with the command or rest provider, not {p:?}"),
        }
    }

    /// Post a comment on a ticket with `jira.comment_cmd` (a draft you
    /// approved). `{id}` is the issue's numeric ID, which twg's comment
    /// command takes; it's looked up only when the template uses it.
    pub fn jira_comment(&self, key: &str, body: &str) -> Result<String> {
        let src = &self.cfg.jira;
        if src.comment_cmd.trim().is_empty() {
            bail!("set jira.comment_cmd to post comments, e.g. twg jira workitem comment create --issue-id {{id}} --body-format markdown --body {{body}} -o json");
        }
        let id = if src.comment_cmd.contains("{id}") {
            let issue = self.jira_issue_raw(key)?;
            issue["id"]
                .as_str()
                .map(String::from)
                .or_else(|| issue["id"].as_u64().map(|n| n.to_string()))
                .ok_or_else(|| anyhow!("{key}: Jira didn't say the issue's id"))?
        } else {
            String::new()
        };
        run_write(
            &src.comment_cmd,
            &[("key", key), ("id", &id), ("body", body)],
        )
    }

    /// Move a ticket with `jira.transition_cmd` (a draft you approved).
    pub fn jira_transition(&self, key: &str, transition: &str, fields: &str) -> Result<String> {
        let src = &self.cfg.jira;
        if src.transition_cmd.trim().is_empty() {
            bail!("set jira.transition_cmd to move tickets, e.g. twg jira workitem transition --id {{key}} --transition-id {{transition}} -o json");
        }
        let fields = if fields.trim().is_empty() {
            "{}"
        } else {
            fields
        };
        run_write(
            &src.transition_cmd,
            &[("key", key), ("transition", transition), ("fields", fields)],
        )
    }

    pub fn jira_get(&self, key: &str) -> Result<Ticket> {
        let src = &self.cfg.jira;
        let key = key.trim().to_uppercase();
        match src.provider.as_str() {
            "rest" => {
                let url = format!(
                    "{}/rest/api/3/issue/{}?fields=summary,status,assignee,priority,issuetype,description,comment",
                    base(src)?,
                    urlencode(&key)
                );
                Ok(jira_ticket(src, &rest_get(src, &url)?))
            }
            "command" => Ok(jira_ticket(src, &command(&src.get_cmd, &[("key", &key)])?)),
            _ => {
                let v = self.ask_agent(&format!(
                    "Use your Jira MCP tools to fetch Jira issue {key}, including its description and the latest 10 comments. Return one JSON object shaped like {TICKET_SHAPE}."
                ))?;
                Ok(serde_json::from_value(v)?)
            }
        }
    }

    // -- confluence -----------------------------------------------------------

    pub fn confluence_search(&self, query: &str) -> Result<Vec<Page>> {
        let src = &self.cfg.confluence;
        match src.provider.as_str() {
            "rest" => {
                let cql = format!("type = page AND text ~ \"{}\"", query.replace('"', "\\\""));
                let url = format!(
                    "{}/wiki/rest/api/search?limit=25&cql={}",
                    base(src)?,
                    urlencode(&cql)
                );
                let v = rest_get(src, &url)?;
                Ok(v["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| Page {
                        id: r["content"]["id"].as_str().unwrap_or("").into(),
                        title: strip_tags(r["title"].as_str().unwrap_or("")),
                        space: r["resultGlobalContainer"]["title"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                        url: format!(
                            "{}/wiki{}",
                            src.base_url.trim_end_matches('/'),
                            r["url"].as_str().unwrap_or("")
                        ),
                        excerpt: strip_tags(r["excerpt"].as_str().unwrap_or("")),
                        body: String::new(),
                    })
                    .collect())
            }
            "command" => Ok(
                serde_json::from_value(command(&src.search_cmd, &[("query", query)])?)
                    .unwrap_or_default(),
            ),
            _ => {
                let v = self.ask_agent(&format!(
                    "Use your Confluence MCP tools to search Confluence for: {query}\nReturn up to 20 pages as a JSON array of objects shaped like {PAGE_SHAPE} with body left empty."
                ))?;
                Ok(serde_json::from_value(v).unwrap_or_default())
            }
        }
    }

    pub fn confluence_get(&self, id: &str) -> Result<Page> {
        let src = &self.cfg.confluence;
        match src.provider.as_str() {
            "rest" => {
                let url = format!(
                    "{}/wiki/rest/api/content/{}?expand=body.storage,space",
                    base(src)?,
                    urlencode(id)
                );
                let v = rest_get(src, &url)?;
                Ok(Page {
                    id: id.into(),
                    title: v["title"].as_str().unwrap_or("").into(),
                    space: v["space"]["name"].as_str().unwrap_or("").into(),
                    url: format!(
                        "{}/wiki{}",
                        src.base_url.trim_end_matches('/'),
                        v["_links"]["webui"].as_str().unwrap_or("")
                    ),
                    excerpt: String::new(),
                    body: v["body"]["storage"]["value"].as_str().unwrap_or("").into(),
                })
            }
            "command" => Ok(serde_json::from_value(command(
                &src.get_cmd,
                &[("id", id)],
            )?)?),
            _ => {
                let v = self.ask_agent(&format!(
                    "Use your Confluence MCP tools to fetch Confluence page {id}. Return one JSON object shaped like {PAGE_SHAPE}, with the full body as markdown."
                ))?;
                Ok(serde_json::from_value(v)?)
            }
        }
    }

    // -- slack ----------------------------------------------------------------

    pub fn slack_search(&self, query: &str) -> Result<Vec<SlackMessage>> {
        let src = &self.cfg.slack;
        match src.provider.as_str() {
            "rest" => {
                let url = format!(
                    "https://slack.com/api/search.messages?count=30&query={}",
                    urlencode(query)
                );
                let token = token(src)?;
                let v: Value = ureq::get(&url)
                    .set("Authorization", &format!("Bearer {token}"))
                    .call()?
                    .into_json()?;
                if v["ok"] != true {
                    bail!("slack: {}", v["error"].as_str().unwrap_or("request failed"));
                }
                Ok(v["messages"]["matches"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| SlackMessage {
                        channel: m["channel"]["name"].as_str().unwrap_or("").into(),
                        user: m["username"].as_str().unwrap_or("").into(),
                        ts: m["ts"].as_str().unwrap_or("").into(),
                        text: m["text"].as_str().unwrap_or("").into(),
                        permalink: m["permalink"].as_str().unwrap_or("").into(),
                    })
                    .collect())
            }
            "command" => Ok(
                serde_json::from_value(command(&src.search_cmd, &[("query", query)])?)
                    .unwrap_or_default(),
            ),
            _ => {
                let v = self.ask_agent(&format!(
                    "Use your Slack MCP tools to search Slack for: {query}\nReturn up to 20 messages as a JSON array of objects shaped like {SLACK_SHAPE}."
                ))?;
                Ok(serde_json::from_value(v).unwrap_or_default())
            }
        }
    }

    /// Free-form question answered by an agent with all its MCPs (Slack,
    /// Jira, Confluence, ...). Returns markdown.
    pub fn ask(&self, question: &str, cwd: Option<&str>) -> Result<String> {
        let out = self.run_query(question, cwd)?;
        Ok(out)
    }

    fn run_query(&self, prompt: &str, cwd: Option<&str>) -> Result<String> {
        use std::io::{BufRead, Write};
        use std::os::unix::process::CommandExt;
        let q = &self.cfg.agent_queries;
        // Claude Code has no wildcard for "every MCP server's tools", so tool
        // permissions go to drovr's lookup policy: read-only tools are
        // approved and anything that writes is denied, whatever the servers
        // are called.
        let mcp = lookup_mcp_config()?;
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--mcp-config",
        ]
        .map(String::from)
        .to_vec();
        args.push(mcp.display().to_string());
        args.extend([
            "--permission-prompt-tool".into(),
            "mcp__drovr__approve".into(),
        ]);
        if !q.model.is_empty() {
            args.extend(["--model".into(), q.model.clone()]);
        }
        let dir = cwd
            .map(std::path::PathBuf::from)
            .unwrap_or_else(crate::config::user_home);
        let log_path = crate::config::home().join("last-lookup.jsonl");
        let mut log = std::fs::File::create(&log_path)?;
        writeln!(log, "{}", json!({ "prompt": prompt, "model": q.model }))?;

        self.say("Starting a lookup agent…");
        let mut child = std::process::Command::new(&self.cfg.claude.command)
            .args(&args)
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| anyhow!("couldn't start {}: {e}", self.cfg.claude.command))?;
        let pid = child.id() as i32;
        RUNNING.lock().map(|mut r| r.push(pid)).ok();
        if let Some(mut stdin) = child.stdin.take() {
            let prompt = prompt.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(prompt.as_bytes());
            });
        }
        let stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut e) = stderr {
                let _ = std::io::Read::read_to_string(&mut e, &mut s);
            }
            s
        });
        // Give up after timeout_seconds.
        let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        {
            let timed_out = timed_out.clone();
            let secs = q.timeout_seconds;
            std::thread::spawn(move || {
                if done_rx.recv_timeout(Duration::from_secs(secs)).is_err() {
                    timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                }
            });
        }

        let mut result: Option<Value> = None;
        for line in std::io::BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
        {
            let _ = writeln!(log, "{line}");
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["type"] == "result" {
                result = Some(v);
            } else if let Some(text) = describe(&v) {
                self.say(&text);
            }
        }
        let status = child.wait()?;
        let _ = done_tx.send(());
        let cancelled = RUNNING.lock().map(|mut r| {
            let was_there = r.contains(&pid);
            r.retain(|p| *p != pid);
            !was_there
        });
        let stderr = err_thread.join().unwrap_or_default();
        let _ = writeln!(
            log,
            "{}",
            json!({ "exit": status.code(), "stderr": stderr })
        );

        if cancelled.unwrap_or(false) {
            bail!("lookup cancelled");
        }
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            bail!(
                "the lookup took longer than {}s and was stopped (agent_queries.timeout_seconds). What it did is in {}",
                q.timeout_seconds,
                log_path.display()
            );
        }
        let Some(v) = result else {
            bail!(
                "claude exited without an answer: {}",
                stderr.trim().chars().take(1000).collect::<String>()
            );
        };
        if v["is_error"] == true {
            bail!("claude: {}", v["result"].as_str().unwrap_or("query failed"));
        }
        let secs = v["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
        self.say(&format!("Done in {secs:.0}s"));
        Ok(v["result"].as_str().unwrap_or("").to_string())
    }

    fn ask_agent(&self, prompt: &str) -> Result<Value> {
        let full = format!("{prompt}\nReply with only the JSON, no prose. If the tools are unavailable, reply with {{\"error\": \"<why>\"}}.");
        let text = self.run_query(&full, None)?;
        let v = extract_json(&text).ok_or_else(|| {
            anyhow!(
                "the agent didn't return JSON: {}",
                text.chars().take(500).collect::<String>()
            )
        })?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            bail!("{err}");
        }
        Ok(v)
    }
}

/// The MCP config that gives a lookup drovr's permission prompt.
fn lookup_mcp_config() -> Result<std::path::PathBuf> {
    let home = crate::config::home();
    let path = home.join("lookup-mcp.json");
    let cfg = json!({
        "mcpServers": {
            "drovr": {
                "type": "stdio",
                "command": crate::engine::locate_cli().display().to_string(),
                "args": ["mcp", "--lookup"],
                "env": { "DROVR_HOME": home.display().to_string() }
            }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
    Ok(path)
}

fn base(src: &Source) -> Result<String> {
    if src.base_url.is_empty() {
        bail!("set base_url for the rest provider in config.toml");
    }
    Ok(src.base_url.trim_end_matches('/').to_string())
}

fn token(src: &Source) -> Result<String> {
    std::env::var(&src.token_env)
        .map_err(|_| anyhow!("environment variable {} is not set", src.token_env))
}

fn auth(src: &Source) -> Result<String> {
    let token = token(src)?;
    if src.email.is_empty() {
        return Ok(format!("Bearer {token}"));
    }
    Ok(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{token}", src.email))
    ))
}

fn rest_get(src: &Source, url: &str) -> Result<Value> {
    Ok(ureq::get(url)
        .set("Authorization", &auth(src)?)
        .set("Accept", "application/json")
        .call()?
        .into_json()?)
}

fn rest_post(src: &Source, url: &str, body: &Value) -> Result<Value> {
    Ok(ureq::post(url)
        .set("Authorization", &auth(src)?)
        .set("Accept", "application/json")
        .send_json(body.clone())?
        .into_json()?)
}

fn command(template: &str, vars: &[(&str, &str)]) -> Result<Value> {
    if template.is_empty() {
        bail!("this source uses the command provider but no command template is configured");
    }
    let cmd = shell::fill(template, vars, &[]);
    let out = shell::sh(&cmd, None, None, Duration::from_secs(60))?.into_stdout(&cmd)?;
    extract_json(&out).ok_or_else(|| anyhow!("`{cmd}` didn't print JSON"))
}

/// Run a write command (a comment, a transition) and return what it printed.
fn run_write(template: &str, vars: &[(&str, &str)]) -> Result<String> {
    let cmd = shell::fill(template, vars, &[]);
    let out = shell::sh(&cmd, None, None, Duration::from_secs(60))?.into_stdout(&cmd)?;
    Ok(out.trim().to_string())
}

/// `command` for something run on a timer: CLIs that honour `DO_NOT_TRACK`
/// (twg does) don't send analytics for it.
fn quiet_command(template: &str, vars: &[(&str, &str)]) -> Result<Value> {
    if template.trim().is_empty() {
        return command(template, vars);
    }
    command(&format!("DO_NOT_TRACK=1 {template}"), vars)
}

/// A bare key like `SRE-123` or free text becomes JQL.
fn as_jql(query: &str) -> String {
    let q = query.trim();
    let is_key = q.split_once('-').is_some_and(|(p, n)| {
        !p.is_empty()
            && p.chars().all(|c| c.is_ascii_alphabetic())
            && n.chars().all(|c| c.is_ascii_digit())
            && !n.is_empty()
    });
    if is_key {
        format!("key = {}", q.to_uppercase())
    } else if q.contains('=') || q.contains('~') || q.to_lowercase().contains(" order by ") {
        q.to_string()
    } else {
        format!(
            "text ~ \"{}\" ORDER BY updated DESC",
            q.replace('"', "\\\"")
        )
    }
}

/// Normalise a Jira REST issue (or anything already ticket-shaped).
/// The issues in what a Jira CLI printed: twg's `{"issues": […]}` from a
/// query or `{"data": […]}` from a get, Jira's own `{"issues": […]}`, or a
/// bare list or issue.
pub fn issue_list(v: Value) -> Vec<Value> {
    match v {
        // Jira's `{"issues": […]}`, and twg's `-o json` envelope,
        // `{"apiVersion", "command", "data": …}`, around either shape.
        Value::Object(mut o) if !o.contains_key("key") => {
            match ["issues", "data", "values"]
                .iter()
                .find_map(|k| o.remove(*k).filter(|v| v.is_array() || v.is_object()))
            {
                Some(inner) => issue_list(inner),
                None => vec![Value::Object(o)],
            }
        }
        Value::Array(items) => items,
        Value::Null => vec![],
        one => vec![one],
    }
}

/// An issue's fields: under `fields` in Jira's REST shape, at the top level
/// in twg's.
pub fn issue_fields(v: &Value) -> &Value {
    if v["fields"].is_object() {
        &v["fields"]
    } else {
        v
    }
}

/// A field that's a name: a string, or an object with `name` or `displayName`.
pub fn name_of(v: &Value) -> String {
    v.as_str()
        .or_else(|| v["name"].as_str())
        .or_else(|| v["displayName"].as_str())
        .unwrap_or("")
        .to_string()
}

fn jira_ticket(src: &Source, v: &Value) -> Ticket {
    if let Some(first) = v["data"].as_array().and_then(|d| d.first()) {
        return jira_ticket(src, first);
    }
    // Already in drovr's own shape (from the agent provider): status is a string.
    if v["fields"].is_null() && !v["status"].is_object() {
        return serde_json::from_value(v.clone()).unwrap_or_default();
    }
    let f = issue_fields(v);
    let key = v["key"].as_str().unwrap_or("").to_string();
    Ticket {
        url: match (v["url"].as_str(), src.base_url.is_empty()) {
            (Some(url), _) => url.to_string(),
            (None, true) => String::new(),
            (None, false) => format!("{}/browse/{key}", src.base_url.trim_end_matches('/')),
        },
        key,
        summary: f["summary"].as_str().unwrap_or("").into(),
        status: name_of(&f["status"]),
        assignee: match name_of(&f["assignee"]) {
            a if a.is_empty() => "Unassigned".into(),
            a => a,
        },
        priority: name_of(&f["priority"]),
        issue_type: match name_of(&f["issuetype"]) {
            t if t.is_empty() => name_of(&f["issueType"]),
            t => t,
        },
        description: match &f["description"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            adf => adf_text(adf),
        },
        comments: f["comment"]["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let body = match &c["body"] {
                    Value::String(s) => s.clone(),
                    adf => adf_text(adf),
                };
                format!(
                    "{}: {}",
                    find_key(&c["author"], &["displayName"]).unwrap_or_default(),
                    body.trim()
                )
            })
            .collect(),
    }
}

/// Flatten Atlassian Document Format into plain text with paragraph breaks.
pub fn adf_text(v: &Value) -> String {
    let mut out = String::new();
    fn walk(v: &Value, out: &mut String) {
        if let Some(t) = v["text"].as_str() {
            out.push_str(t);
        }
        let kind = v["type"].as_str().unwrap_or("");
        if kind == "listItem" {
            out.push_str("- ");
        }
        for child in v["content"].as_array().into_iter().flatten() {
            walk(child, out);
        }
        if matches!(
            kind,
            "paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote" | "hardBreak"
        ) {
            out.push('\n');
        }
    }
    walk(v, &mut out);
    out.trim().to_string()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("@@@hl@@@", "").replace("@@@endhl@@@", "")
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The prompt an agent gets when a ticket is handed to it.
pub fn ticket_task(t: &Ticket, extra: &str, you: &str) -> String {
    let mut s = format!("Work on Jira ticket {}: {}\n", t.key, t.summary);
    if !t.url.is_empty() {
        s += &format!("Link: {}\n", t.url);
    }
    s += &format!(
        "Type: {} · Status: {} · Priority: {}\n",
        t.issue_type, t.status, t.priority
    );
    if !t.description.is_empty() {
        s += &format!("\n## Description\n{}\n", t.description);
    }
    if !t.comments.is_empty() {
        s += "\n## Recent comments\n";
        for c in t.comments.iter().take(10) {
            s += &format!("- {c}\n");
        }
    }
    if !extra.trim().is_empty() {
        s += &format!("\n## Notes from {you}\n{extra}\n");
    }
    let s = s + "\nUse your Jira, Confluence and Slack MCPs for more context when it helps. Ask me (ask_human) before anything ambiguous or irreversible.";
    crate::config::personalize(&s, you)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_notes_name_you() {
        let t = Ticket {
            key: "SRE-3253".into(),
            summary: "Bun 1.4".into(),
            ..Ticket::default()
        };
        let task = ticket_task(&t, "Don't touch the harness yet", "Izz");
        assert!(
            task.contains("## Notes from Izz\nDon't touch the harness yet"),
            "{task}"
        );
        assert!(!ticket_task(&t, "", "Izz").contains("Notes from"));
    }

    #[test]
    fn jql_from_input() {
        assert_eq!(as_jql("pay-123"), "key = PAY-123");
        assert_eq!(as_jql("project = PAY"), "project = PAY");
        assert_eq!(
            as_jql("refund bug"),
            "text ~ \"refund bug\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn adf_to_text() {
        let adf: Value = serde_json::from_str(
            r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"bulletList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"one"}]}]}]}]}"#,
        )
        .unwrap();
        assert_eq!(adf_text(&adf), "Hello\n- one");
    }

    #[test]
    fn normalises_rest_issue() {
        let src = Source {
            base_url: "https://x.atlassian.net".into(),
            ..Source::default()
        };
        let v: Value = serde_json::from_str(r#"{"key":"PAY-1","fields":{"summary":"S","status":{"name":"To Do"},"assignee":null,"description":null}}"#).unwrap();
        let t = jira_ticket(&src, &v);
        assert_eq!(t.status, "To Do");
        assert_eq!(t.assignee, "Unassigned");
        assert_eq!(t.url, "https://x.atlassian.net/browse/PAY-1");
    }

    #[test]
    fn reads_twg_output() {
        let src = Source::default();
        // `twg jira workitem get SRE-3180 -o json`, trimmed.
        let get: Value = serde_json::from_str(
            r#"{"apiVersion":"v2","command":"jira.workitem.get","data":[{"id":"339768","key":"SRE-3180",
              "issuetype":{"name":"Task"},"summary":"Build an AI release readiness check",
              "description":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Why"}]}]},
              "comment":{"comments":[{"author":{"displayName":"Parker"},"body":"Looks good"}],"total":1},
              "assignee":{"displayName":"Izz Noland"},"priority":{"name":"Medium/P3"},
              "status":{"name":"In Progress","statusCategory":{"key":"indeterminate"}},
              "url":"https://yourco.atlassian.net/browse/SRE-3180"}],"meta":{}}"#,
        )
        .unwrap();
        let t = jira_ticket(&src, &get);
        assert_eq!(
            (
                t.key.as_str(),
                t.status.as_str(),
                t.assignee.as_str(),
                t.priority.as_str(),
                t.issue_type.as_str()
            ),
            ("SRE-3180", "In Progress", "Izz Noland", "Medium/P3", "Task")
        );
        assert_eq!(t.url, "https://yourco.atlassian.net/browse/SRE-3180");
        assert_eq!(t.description, "Why");
        assert_eq!(t.comments, ["Parker: Looks good"]);
        // `twg jira workitem query --jql …`, with its progress line.
        let out = "◇  Completed jira.workitem.query.\n{\"issues\":[{\"key\":\"SRE-3314\",\"summary\":\"Sync\",\"status\":{\"name\":\"Complete\",\"statusCategory\":{\"key\":\"done\"}},\"issueType\":{\"name\":\"Task\"},\"assignee\":{\"displayName\":\"Izz Noland\"}}]}";
        let list = issue_list(extract_json(out).unwrap());
        let t = jira_ticket(&src, &list[0]);
        assert_eq!(
            (t.key.as_str(), t.status.as_str(), t.issue_type.as_str()),
            ("SRE-3314", "Complete", "Task")
        );
        // With `-o json`, twg wraps either shape in an envelope.
        for out in [
            json!({ "apiVersion": "v2", "command": "jira.workitem.query", "data": { "issues": [{ "key": "SRE-1" }, { "key": "SRE-2" }] } }),
            json!({ "apiVersion": "v2", "command": "jira.workitem.query", "data": [{ "key": "SRE-1" }, { "key": "SRE-2" }] }),
            json!({ "data": { "issues": [], "total": 0 } }),
        ] {
            let keys: Vec<_> = issue_list(out)
                .iter()
                .map(|i| i["key"].as_str().unwrap_or("").to_string())
                .collect();
            assert!(keys.is_empty() || keys == ["SRE-1", "SRE-2"], "{keys:?}");
        }
        // One issue's own `data`-free object stays one issue.
        assert_eq!(
            issue_list(json!({ "key": "SRE-9", "issues": "n/a" })).len(),
            1
        );
        // drovr's own shape still reads as it is.
        let own = jira_ticket(&src, &json!({ "key": "A-1", "status": "Done" }));
        assert_eq!(own.status, "Done");
    }
}
