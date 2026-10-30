//! The `drovr` MCP server each agent gets (stdio, newline-delimited JSON-RPC).
//!
//! It's how agents coordinate with each other and with the human: messages,
//! a shared blackboard, questions, proposed commands, worker spawning, and
//! the permission prompt for headless runs.

use crate::engine::{answer_text, Engine};
use crate::store::{now, Item};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::time::Duration;

const PROTOCOL: &str = "2025-06-18";

/// The tools, worded for you: `ask_human` becomes `ask_<your name>`.
/// Tools only agents get with `workflow.enabled`.
const WORKFLOW_TOOLS: &[&str] = &[
    "record_decision",
    "draft_jira_update",
    "handoff",
    "checkpoint",
    "submit_proposals",
];
/// Of those, the ones only for groups that work a ticket in stages.
const STAGE_TOOLS: &[&str] = &["handoff", "checkpoint"];

/// `staged`: the agent's group works its ticket in stages (see
/// `Engine::staged`).
fn tools(cfg: &crate::config::Config, staged: bool) -> Value {
    let mut all = base_tools();
    let Some(list) = all.as_array_mut() else {
        return all;
    };
    // Only once you've set up how drovr reads and writes Confluence pages.
    if cfg.confluence.page_get_cmd.trim().is_empty() {
        list.retain(|t| t["name"] != "publish_confluence");
    }
    // These belong to the ticket workflow.
    if !cfg.workflow.enabled {
        list.retain(|t| !WORKFLOW_TOOLS.contains(&t["name"].as_str().unwrap_or("")));
    }
    if !staged {
        list.retain(|t| !STAGE_TOOLS.contains(&t["name"].as_str().unwrap_or("")));
        // spawn_worker as it was before stages.
        for t in list.iter_mut().filter(|t| t["name"] == "spawn_worker") {
            if let Some(p) = t["inputSchema"]["properties"].as_object_mut() {
                p.remove("stage");
                p.remove("model");
            }
            t["inputSchema"]["required"] = json!(["name", "task"]);
        }
    }
    serde_json::from_str(&crate::config::personalize(
        &all.to_string(),
        &cfg.your_name(),
    ))
    .unwrap_or(all)
}

fn base_tools() -> Value {
    let s = |props: Value, required: &[&str]| json!({ "type": "object", "properties": props, "required": required });
    json!([
        { "name": "whoami", "description": "Your agent name, role, group, working directory and ticket.", "inputSchema": s(json!({}), &[]) },
        { "name": "list_agents", "description": "Agents in your group (or all agents) with their state, status and task.",
          "inputSchema": s(json!({ "all": { "type": "boolean", "description": "List every agent, not just your group." } }), &[]) },
        { "name": "send_message", "description": "Message another agent. `to` is an agent name, `@group` for everyone in your group, `orchestrator` for your group's lead, or `human` to message the human you work for.",
          "inputSchema": s(json!({ "to": { "type": "string" }, "body": { "type": "string" } }), &["to", "body"]) },
        { "name": "read_messages", "description": "Unread messages for you. Set wait_seconds to block until one arrives (max 600).",
          "inputSchema": s(json!({ "wait_seconds": { "type": "integer" } }), &[]) },
        { "name": "set_note", "description": "Write a key on your group's shared blackboard (file ownership, contracts, decisions).",
          "inputSchema": s(json!({ "key": { "type": "string" }, "value": { "type": "string" } }), &["key", "value"]) },
        { "name": "get_notes", "description": "Read your group's shared blackboard.", "inputSchema": s(json!({}), &[]) },
        { "name": "publish_confluence", "description": "Propose a Confluence page: new content for an existing `page`, or a new page under `parent` (a page or folder), wherever the human told you. They review a diff against the live page and the full copy; approved, it's saved as a Confluence draft, never published, and you're told. Returns straight away. The body is the whole page as HTML (Confluence's preferred format); don't repeat the title as a heading. To change an existing page, start from its current HTML (`twg confluence content get <page> --draft --detail full --format html -o json`, `data.body.value`) and keep what you aren't changing, panels, layouts and data-local-id attributes included.",
          "inputSchema": s(json!({
              "title": { "type": "string" },
              "body": { "type": "string", "description": "The whole page, as HTML" },
              "page": { "type": "string", "description": "ID or URL of the page to update" },
              "parent": { "type": "string", "description": "ID or URL of the page or folder to create it under" },
              "note": { "type": "string", "description": "What changed and why, for the human" }
          }), &["title", "body"]) },
        { "name": "set_ticket", "description": "Take the Jira ticket your work tracks, such as one you just created for your task. drovr records your work under it from then on.",
          "inputSchema": s(json!({ "key": { "type": "string", "description": "The ticket's key, e.g. SRE-123" } }), &["key"]) },
        { "name": "set_status", "description": "One-line summary of what you're doing, shown to the human.",
          "inputSchema": s(json!({ "status": { "type": "string" } }), &["status"]) },
        { "name": "ask_human", "description": "Ask the human to decide something. Offer 2-4 concrete options and say which you recommend. Waits up to wait_seconds (default 300) for an answer; if none arrives, end your turn and you'll be woken with it.",
          "inputSchema": s(json!({
              "question": { "type": "string" },
              "options": { "type": "array", "items": { "type": "string" } },
              "context": { "type": "string", "description": "Background the human needs, in markdown." },
              "wait_seconds": { "type": "integer" } }), &["question"]) },
        { "name": "propose_command", "description": "Propose a shell command for the human to approve and run (deploys, migrations, anything risky or outside your sandbox). Returns its output once run.",
          "inputSchema": s(json!({
              "command": { "type": "string" },
              "reason": { "type": "string" },
              "cwd": { "type": "string", "description": "Defaults to your working directory." },
              "wait_seconds": { "type": "integer" } }), &["command", "reason"]) },
        { "name": "record_decision", "description": "Record a decision you made yourself: the question, the options you weighed, what you chose and why. It goes into the ticket's record, which the human reviews later. Decisions the human makes through ask_human are recorded already.",
          "inputSchema": s(json!({
              "question": { "type": "string" },
              "options": { "type": "array", "items": { "type": "string" } },
              "chosen": { "type": "string" },
              "rationale": { "type": "string" } }), &["question", "chosen"]) },
        { "name": "draft_jira_update", "description": "Draft a Jira update for the human to review and post: a comment (markdown) and/or a move to another status. Nothing is posted until they approve it in drovr. Use it for progress the ticket's watchers should see, not for questions (use ask_human for those).",
          "inputSchema": s(json!({
              "comment": { "type": "string" },
              "transition": { "type": "string", "description": "The status to move to, e.g. In Progress or Complete." },
              "fields": { "type": "object", "description": "Fields the move needs, by field ID or name (e.g. a resolution)." },
              "ticket": { "type": "string", "description": "Defaults to your ticket." } }), &[]) },
        { "name": "submit_proposals", "description": "Retro agents only: submit what you'd change, each with its evidence. The human approves them one by one in drovr.",
          "inputSchema": s(json!({
              "proposals": { "type": "array", "items": { "type": "object", "properties": {
                  "kind": { "type": "string", "enum": ["model", "threshold", "wake_rule", "process", "skill_edit", "new_skill"] },
                  "title": { "type": "string" },
                  "why": { "type": "string" },
                  "evidence": { "type": "array", "items": { "type": "string" } },
                  "change": { "type": "object", "description": "{\"config\": {\"key\", \"value\"}}, {\"skill\": {\"name\", \"content\"}}, or {} for a change the human makes." } },
                  "required": ["kind", "title", "evidence", "change"] } } }), &["proposals"]) },
        { "name": "check_answer", "description": "Check (or wait for) the answer to an earlier ask_human or propose_command.",
          "inputSchema": s(json!({ "id": { "type": "integer" }, "wait_seconds": { "type": "integer" } }), &["id"]) },
        { "name": "spawn_worker", "description": "Orchestrators only: start a worker agent in your group with a precise task.",
          "inputSchema": s(json!({
              "name": { "type": "string", "description": "Short name, e.g. api or tests." },
              "task": { "type": "string" },
              "cwd": { "type": "string", "description": "Defaults to your working directory." },
              "stage": { "type": "string", "description": "The workflow stage this agent runs (triage, research, blueprint, build, close). Names it after the stage unless you give a name, and runs it on the stage's model." },
              "model": { "type": "string", "description": "The model for this agent, when the human chose one for its stage." } }), &["task"]) },
        { "name": "handoff", "description": "Stage agents only: end your stage by handing off to the next. drovr saves it in the ticket's folder and the human approves it (or sends it back with notes). After calling it, end your turn.",
          "inputSchema": s(json!({
              "summary": { "type": "string", "description": "What this stage found or produced." },
              "outputs": { "type": "string", "description": "Reports, files, branches, PRs, with links or paths." },
              "decisions": { "type": "string", "description": "Decisions made in this stage, and why." },
              "open_questions": { "type": "string", "description": "What the next stage must resolve." },
              "next_inputs": { "type": "string", "description": "Exactly what the next stage should start from." } }), &["summary", "next_inputs"]) },
        { "name": "checkpoint", "description": "Build stage: when drovr asks, write down where you are, so you continue in a fresh session from it. After calling it, end your turn.",
          "inputSchema": s(json!({
              "done": { "type": "string" },
              "left": { "type": "string" },
              "state": { "type": "string", "description": "The branch, PR and tests." },
              "decisions": { "type": "string", "description": "Decisions made since the blueprint." } }), &["done", "left"]) },
        { "name": "stop_agent", "description": "Orchestrators only: stop a worker in your group.",
          "inputSchema": s(json!({ "name": { "type": "string" } }), &["name"]) },
        { "name": "publish_report", "description": "Publish an HTML page for the human to review: findings, plans, designs, comparisons, summaries, anything richer than a short message. It is saved to their plans folder (in your group's subfolder) and shows up in drovr's Reports view. Pass either `html` (a complete, self-contained document with inline CSS) or `path` to an .html file you wrote, which is copied there. Publishing a title you published before replaces that report, so revise a plan or runbook under the same title rather than a new one.",
          "inputSchema": s(json!({
              "title": { "type": "string" },
              "summary": { "type": "string", "description": "One or two sentences shown in the list." },
              "html": { "type": "string" },
              "path": { "type": "string", "description": "Absolute, or relative to your working directory." } }), &["title"]) },
        { "name": "report_done", "description": "Report that your task is finished, with a summary of what changed and what's left.",
          "inputSchema": s(json!({ "summary": { "type": "string" } }), &["summary"]) },
        { "name": "approve", "description": "Permission prompt used by drovr for headless runs.",
          "inputSchema": s(json!({ "tool_name": { "type": "string" }, "input": { "type": "object" }, "tool_use_id": { "type": "string" } }), &["tool_name"]) }
    ])
}

fn approve_only() -> Value {
    Value::Array(
        base_tools()
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["name"] == "approve")
            .cloned()
            .collect(),
    )
}

pub struct Server {
    engine: Engine,
    me: String,
    /// Serving a one-shot lookup (`drovr mcp --lookup`): only the permission
    /// prompt, approving read-only tools.
    lookup: bool,
}

/// Whether a tool only reads, judged from its name: `searchJiraIssuesUsingJql`
/// and `slack_read_channel` do, `createJiraIssue` and `slack_send_message`
/// don't. Lookups get these without asking; everything else is denied.
pub fn read_only_tool(tool: &str) -> bool {
    const READ: &[&str] = &[
        "get", "list", "search", "fetch", "read", "lookup", "query", "find", "describe", "view",
        "show", "retrieve",
    ];
    const WRITE: &[&str] = &[
        "create",
        "update",
        "delete",
        "add",
        "remove",
        "send",
        "post",
        "edit",
        "set",
        "write",
        "put",
        "patch",
        "transition",
        "assign",
        "upload",
        "move",
        "archive",
        "invite",
        "react",
        "schedule",
        "close",
        "merge",
        "comment",
        "publish",
        "submit",
        "approve",
        "reject",
        "cancel",
        "run",
        "execute",
        "link",
        "unlink",
    ];
    // Only MCP tools; a built-in tool that asks (Bash, Write…) is never read-only here.
    let Some((_, name)) = mcp_parts(tool) else {
        return false;
    };
    let words = words_of(name);
    words.iter().any(|w| READ.contains(&w.as_str()))
        && !words.iter().any(|w| WRITE.contains(&w.as_str()))
}

/// Whether an MCP tool can reach any web address: generic fetchers, browsers
/// and scrapers. Lookups read text anyone can write (a Slack message, a
/// ticket), so a lookup that could also call one of these could be talked
/// into sending what it read to an address in that text.
pub fn web_tool(tool: &str) -> bool {
    const SERVERS: &[&str] = &[
        "fetch",
        "browser",
        "browserbase",
        "puppeteer",
        "playwright",
        "firecrawl",
        "scrape",
        "scraper",
        "crawl",
        "crawler",
        "web",
        "http",
    ];
    const WORDS: &[&str] = &[
        "url", "urls", "uri", "http", "https", "browse", "browser", "navigate", "scrape", "crawl",
        "web", "webpage",
    ];
    let Some((server, name)) = mcp_parts(tool) else {
        return false;
    };
    words_of(server)
        .iter()
        .any(|w| SERVERS.contains(&w.as_str()))
        || words_of(name).iter().any(|w| WORDS.contains(&w.as_str()))
}

/// `mcp__<server>__<tool>` split into server and tool.
fn mcp_parts(tool: &str) -> Option<(&str, &str)> {
    let rest = tool.strip_prefix("mcp__")?;
    Some(rest.rsplit_once("__").unwrap_or((rest, rest)))
}

/// `searchJiraIssues` or `slack_read_channel` as lowercase words.
fn words_of(name: &str) -> Vec<String> {
    let mut words = vec![];
    let mut word = String::new();
    for c in name.chars() {
        if (c == '_' || c == '-' || c == '.' || c.is_uppercase()) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if c.is_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn wait_for(engine: &Engine, id: i64, secs: u64) -> Result<Item> {
    let deadline = now() + secs.min(3600) as f64;
    loop {
        let item = engine
            .store
            .item(id)?
            .ok_or_else(|| anyhow!("no inbox item {id}"))?;
        if item.status != "pending" || now() >= deadline {
            return Ok(item);
        }
        std::thread::sleep(Duration::from_millis(750));
    }
}

fn matches(pattern: &str, tool: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool.starts_with(prefix),
        None => pattern == tool,
    }
}

impl Server {
    pub fn new(engine: Engine, me: String) -> Server {
        Server {
            engine,
            me,
            lookup: false,
        }
    }

    /// The server for a one-shot lookup, which only answers permission prompts.
    pub fn lookup(engine: Engine) -> Server {
        Server {
            engine,
            me: String::new(),
            lookup: true,
        }
    }

    pub fn serve(&self) -> Result<()> {
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Value>(&line) {
                Ok(req) => self.handle(&req),
                Err(e) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
                ),
            };
            if let Some(resp) = resp {
                writeln!(stdout, "{resp}")?;
                stdout.flush()?;
            }
        }
        Ok(())
    }

    /// Whether this agent's group works its ticket in stages.
    fn staged(&self) -> bool {
        self.engine
            .store
            .agent(&self.me)
            .ok()
            .flatten()
            .is_some_and(|a| self.engine.staged(&a))
    }

    pub fn handle(&self, req: &Value) -> Option<Value> {
        let id = req.get("id")?.clone();
        let method = req["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "drovr", "version": env!("CARGO_PKG_VERSION") }
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(
                json!({ "tools": if self.lookup { approve_only() } else { tools(&self.engine.cfg, self.staged()) } }),
            ),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"]["arguments"].clone();
                Ok(match self.call(name, &args) {
                    Ok(v) => json!({ "content": [{ "type": "text", "text": text_of(v) }] }),
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": format!("error: {e}") }], "isError": true })
                    }
                })
            }
            _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        })
    }

    pub fn call(&self, tool: &str, a: &Value) -> Result<Value> {
        let result = self.call_tool(tool, a);
        if let (Ok(r), false) = (&result, self.lookup) {
            self.record_call(tool, a, r);
        }
        result
    }

    /// Add what an agent did to its ticket's record. Tools that ask the
    /// human record their Inbox item themselves, before they wait.
    fn record_call(&self, tool: &str, a: &Value, result: &Value) {
        let text = |k: &str, max: usize| crate::engine::truncate(a[k].as_str().unwrap_or(""), max);
        let (kind, data) = match tool {
            "send_message" => (
                "message",
                json!({ "to": result["to"], "body": text("body", 4000) }),
            ),
            "set_status" => ("status", json!({ "status": text("status", 500) })),
            "set_note" => (
                "note",
                json!({ "key": text("key", 200), "value": text("value", 4000) }),
            ),
            "publish_report" => (
                "report",
                json!({ "title": text("title", 500), "summary": text("summary", 1000), "path": result["published"] }),
            ),
            "record_decision" => (
                "decision",
                json!({
                    "by": "agent",
                    "question": text("question", 1000),
                    "options": a["options"],
                    "chosen": text("chosen", 1000),
                    "rationale": text("rationale", 4000),
                }),
            ),
            _ => return,
        };
        self.engine.record(&self.me, kind, data);
    }

    fn call_tool(&self, tool: &str, a: &Value) -> Result<Value> {
        let st = &self.engine.store;
        if self.lookup {
            return match tool {
                "approve" => Ok(self.approve_lookup(a)),
                other => bail!("{other} isn't available to lookups"),
            };
        }
        let me = self.engine.agent(&self.me)?;
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        let wait = |default: u64| a["wait_seconds"].as_u64().unwrap_or(default);
        match tool {
            "whoami" => Ok(json!({
                "name": me.name, "role": me.role, "group": me.group_name, "cwd": me.cwd, "ticket": me.ticket, "task": me.task
            })),
            "list_agents" => {
                let group = if a["all"] == true || me.group_name.is_empty() {
                    None
                } else {
                    Some(me.group_name.as_str())
                };
                let agents: Vec<Value> = st
                    .agents(group)?
                    .iter()
                    .map(|x| json!({
                        "name": x.name, "role": x.role, "group": x.group_name, "state": self.engine.display_state(x),
                        "status": x.status, "task": x.task.lines().next().unwrap_or(""), "cwd": x.cwd
                    }))
                    .collect();
                Ok(json!(agents))
            }
            "send_message" => {
                let mut to = s("to");
                let body = s("body");
                if body.is_empty() {
                    bail!("body is empty");
                }
                // However the agent spells "the human", it reaches you.
                if self.engine.cfg.is_you(&to) {
                    to = "human".into();
                }
                if to == "orchestrator" || to == "lead" {
                    to = st
                        .agents(Some(&me.group_name))?
                        .into_iter()
                        .find(|x| x.role == "orchestrator")
                        .map(|x| x.name)
                        .ok_or_else(|| anyhow!("your group has no orchestrator"))?;
                }
                let targets: Vec<String> = match to.as_str() {
                    "@group" => st
                        .agents(Some(&me.group_name))?
                        .into_iter()
                        .map(|x| x.name)
                        .filter(|n| *n != me.name)
                        .collect(),
                    "human" | "*" => vec![],
                    name => {
                        st.agent(name)?.ok_or_else(|| {
                            anyhow!("no agent named {name}; call list_agents, or use `human` to message {}", self.engine.cfg.your_name())
                        })?;
                        vec![name.to_string()]
                    }
                };
                let id = st.send(&me.name, &to, &body, &me.group_name)?;
                for t in &targets {
                    self.engine.maybe_wake(t)?;
                }
                Ok(json!({ "sent": id, "to": to }))
            }
            "read_messages" => {
                let deadline = now() + wait(0).min(600) as f64;
                loop {
                    let msgs = st.unread(&me.name, &me.group_name)?;
                    if !msgs.is_empty() || now() >= deadline {
                        if let Some(last) = msgs.last() {
                            st.mark_read(&me.name, last.id)?;
                        }
                        let answers: Vec<String> = st
                            .undelivered_answers(&me.name)?
                            .iter()
                            .map(|i| {
                                let _ = st.mark_delivered(i.id);
                                answer_text(i, &self.engine.cfg.your_name())
                            })
                            .collect();
                        let cfg = &self.engine.cfg;
                        let msgs: Vec<Value> = msgs
                            .iter()
                            .map(|m| json!({ "from": cfg.display_name(&m.sender), "to": cfg.display_name(&m.recipient), "body": m.body }))
                            .collect();
                        return Ok(json!({ "messages": msgs, "answers": answers }));
                    }
                    std::thread::sleep(Duration::from_millis(750));
                }
            }
            "set_note" => {
                st.set_note(
                    &group_or_self(&me.group_name, &me.name),
                    &s("key"),
                    &s("value"),
                    &me.name,
                )?;
                Ok(json!("saved"))
            }
            "get_notes" => Ok(serde_json::to_value(
                st.notes(&group_or_self(&me.group_name, &me.name))?,
            )?),
            "set_status" => {
                st.set_agent(&me.name, "status", &s("status"))?;
                Ok(json!("ok"))
            }
            "publish_confluence" => crate::confluence::propose(&self.engine, &me, a),
            "set_ticket" => Ok(json!(self.engine.set_ticket(&me, &s("key"))?)),
            // `ask_<your name>`, or `ask_human` from sessions started before.
            // Under any name it's had: `ask_human`, or `ask_<your name>` from
            // before you set or changed `defaults.your_name`.
            n if n.starts_with("ask_") => {
                let options: Vec<String> = a["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o.as_str().map(String::from))
                    .collect();
                let id = st.add_item(
                    &me.name,
                    "decision",
                    &s("question"),
                    &s("context"),
                    &options,
                    &json!({}),
                )?;
                self.engine.record_item(id);
                self.reply(wait_for(&self.engine, id, wait(300))?)
            }
            "propose_command" => {
                let cwd = if s("cwd").is_empty() {
                    me.cwd.clone()
                } else {
                    s("cwd")
                };
                let payload = json!({ "command": s("command"), "cwd": cwd });
                let id = st.add_item(
                    &me.name,
                    "command",
                    &s("command"),
                    &s("reason"),
                    &[],
                    &payload,
                )?;
                self.engine.record_item(id);
                self.reply(wait_for(&self.engine, id, wait(300))?)
            }
            "check_answer" => {
                let id = a["id"].as_i64().ok_or_else(|| anyhow!("id is required"))?;
                // Check before waiting, so nobody waits on (or reads) another agent's item.
                if st.item(id)?.is_some_and(|i| i.agent != me.name) {
                    bail!("item {id} isn't yours");
                }
                self.reply(wait_for(&self.engine, id, wait(0))?)
            }
            "spawn_worker" => {
                let (stage, model) = if self.engine.staged(&me) {
                    (s("stage"), s("model"))
                } else {
                    (String::new(), String::new())
                };
                self.engine.spawn_worker(
                    &me.name,
                    &s("name"),
                    &s("task"),
                    &s("cwd"),
                    &stage,
                    &model,
                )
            }
            "handoff" | "checkpoint" | "submit_proposals" if !self.engine.cfg.workflow.enabled => {
                bail!("{tool} is off: the ticket workflow isn't enabled")
            }
            "handoff" => self.engine.handoff(&me, a),
            "checkpoint" => self.engine.checkpoint(&me, a),
            "submit_proposals" => self.engine.submit_proposals(&me, a),
            "stop_agent" => {
                let target = self.engine.agent(&s("name"))?;
                if me.role != "orchestrator" || target.group_name != me.group_name {
                    bail!("you can only stop workers in your own group");
                }
                self.engine.stop(&target.name)?;
                self.engine
                    .record(&target.name, "stopped", json!({ "by": me.name }));
                Ok(json!("stopped"))
            }
            "publish_report" => {
                let title = s("title");
                if title.is_empty() {
                    bail!("title is required");
                }
                let html = if !s("html").is_empty() {
                    s("html")
                } else if !s("path").is_empty() {
                    let p = std::path::Path::new(&me.cwd).join(s("path"));
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if !p.is_file() || !(ext == "html" || ext == "htm") {
                        bail!("{} is not an existing .html file", p.display());
                    }
                    std::fs::read_to_string(&p)?
                } else {
                    bail!("pass either html or path");
                };
                let folder = self.engine.plans_folder(&me.group_name)?;
                let path = crate::reports::publish(
                    &self.engine.cfg,
                    st,
                    &folder,
                    &me.name,
                    &title,
                    &s("summary"),
                    &html,
                )?;
                Ok(json!({ "published": path }))
            }
            "report_done" => {
                let summary = s("summary");
                st.set_agent(
                    &me.name,
                    "status",
                    &format!("done: {}", summary.lines().next().unwrap_or("")),
                )?;
                let id = st.add_item(
                    &me.name,
                    "done",
                    &format!("{} finished", me.name),
                    &summary,
                    &[],
                    &json!({}),
                )?;
                self.engine.record_item(id);
                if me.role == "worker" {
                    if let Some(lead) = st
                        .agents(Some(&me.group_name))?
                        .into_iter()
                        .find(|x| x.role == "orchestrator")
                    {
                        st.send(
                            &me.name,
                            &lead.name,
                            &format!("Done. {summary}"),
                            &me.group_name,
                        )?;
                        self.engine.maybe_wake(&lead.name)?;
                    }
                }
                Ok(json!("reported; you can end your turn"))
            }
            "record_decision" => {
                if !self.engine.cfg.workflow.enabled {
                    bail!("record_decision is off: the ticket workflow isn't enabled");
                }
                if s("question").trim().is_empty() || s("chosen").trim().is_empty() {
                    bail!("question and chosen are required");
                }
                Ok(json!(if crate::ledger::tickets_in(&me.ticket).is_empty()
                    && self
                        .engine
                        .store
                        .group(&me.group_name)?
                        .is_none_or(|g| crate::ledger::tickets_in(&g.ticket).is_empty())
                {
                    "noted, but you have no ticket, so there's no record to add it to"
                } else {
                    "recorded"
                }))
            }
            "draft_jira_update" => {
                if !self.engine.cfg.workflow.enabled {
                    bail!("draft_jira_update is off: the ticket workflow isn't enabled");
                }
                let mine = {
                    let own = crate::ledger::tickets_in(&me.ticket);
                    if own.is_empty() {
                        st.group(&me.group_name)?
                            .map(|g| crate::ledger::tickets_in(&g.ticket))
                            .unwrap_or_default()
                    } else {
                        own
                    }
                };
                let ticket = match crate::ledger::tickets_in(&s("ticket")).into_iter().next() {
                    Some(t) => t,
                    None => mine
                        .first()
                        .cloned()
                        .ok_or_else(|| anyhow!("you have no ticket; pass ticket"))?,
                };
                let fields = if a["fields"].is_object() {
                    a["fields"].to_string()
                } else {
                    String::new()
                };
                let mut drafted = vec![];
                if !s("comment").trim().is_empty() {
                    drafted.push(crate::rules::draft_jira(
                        &self.engine,
                        &me.name,
                        &ticket,
                        "comment",
                        &s("comment"),
                        "",
                        "",
                    )?);
                }
                if !s("transition").trim().is_empty() {
                    drafted.push(crate::rules::draft_jira(
                        &self.engine,
                        &me.name,
                        &ticket,
                        "transition",
                        "",
                        &s("transition"),
                        &fields,
                    )?);
                }
                if drafted.is_empty() {
                    bail!("pass a comment, a transition, or both");
                }
                Ok(
                    json!({ "drafted": drafted, "note": "The human reviews and posts these in drovr; you don't need to wait." }),
                )
            }
            "approve" => self.approve(&me.name, a),
            other => bail!("unknown tool {other}"),
        }
    }

    fn reply(&self, item: Item) -> Result<Value> {
        if item.status == "pending" {
            return Ok(json!({
                "id": item.id,
                "status": "pending",
                "note": "No answer yet. Continue with other work or end your turn; you'll be woken with the answer. You can also call check_answer."
            }));
        }
        self.engine.store.mark_delivered(item.id)?;
        Ok(
            json!({ "id": item.id, "status": item.status, "answer": answer_text(&item, &self.engine.cfg.your_name()) }),
        )
    }

    /// Lookups run unattended, so nothing reaches the Inbox: read-only MCP
    /// tools that can't reach the web, from `agent_queries.servers` when
    /// that's set, are approved, and so is `agent_queries.auto_allow`. The
    /// rest are denied.
    fn approve_lookup(&self, a: &Value) -> Value {
        let tool = a["tool_name"].as_str().unwrap_or("");
        let q = &self.engine.cfg.agent_queries;
        let server_ok = q.servers.is_empty()
            || mcp_parts(tool).is_some_and(|(server, _)| {
                let server = server.to_lowercase();
                q.servers.iter().any(|s| server.contains(&s.to_lowercase()))
            });
        let allowed = q.auto_allow.iter().any(|p| matches(p, tool))
            || (read_only_tool(tool) && !web_tool(tool) && server_ok);
        if allowed {
            json!({ "behavior": "allow", "updatedInput": a["input"].clone() })
        } else {
            json!({
                "behavior": "deny",
                "message": format!("{tool} isn't allowed in a drovr lookup, which may only read. Answer with what you have, or report the error.")
            })
        }
    }

    fn approve(&self, me: &str, a: &Value) -> Result<Value> {
        let tool = a["tool_name"].as_str().unwrap_or("");
        let input = a["input"].clone();
        let allow = json!({ "behavior": "allow", "updatedInput": input });
        if self
            .engine
            .cfg
            .permissions
            .auto_allow
            .iter()
            .any(|p| matches(p, tool))
        {
            return Ok(allow);
        }
        let summary = ["command", "file_path", "url", "pattern"]
            .iter()
            .find_map(|k| input[*k].as_str().map(String::from))
            .unwrap_or_default();
        let body = format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(&input).unwrap_or_default()
        );
        let options = vec!["allow".to_string(), "deny".to_string()];
        let id = self.engine.store.add_item(
            me,
            "permission",
            &format!("{tool} {summary}"),
            &body,
            &options,
            &json!({ "tool": tool }),
        )?;
        self.engine.record_item(id);
        let item = wait_for(&self.engine, id, self.engine.cfg.permissions.wait_seconds)?;
        self.engine.store.mark_delivered(id)?;
        if item.status == "answered" && item.answer.starts_with("allow") {
            return Ok(allow);
        }
        if item.status == "pending" {
            self.engine
                .store
                .resolve_item(id, "dismissed", "timed out")?;
            self.engine.record_item(id);
        }
        let you = self.engine.cfg.your_name();
        let reason = match item.status.as_str() {
            "pending" => format!(
                "{} didn't respond in time.",
                crate::engine::capitalize(&you)
            ),
            _ if !item.answer.is_empty() && item.answer != "deny" => {
                format!("Denied by {you}: {}", item.answer)
            }
            _ => format!("Denied by {you}."),
        };
        Ok(json!({ "behavior": "deny", "message": reason }))
    }
}

fn group_or_self(group: &str, me: &str) -> String {
    if group.is_empty() {
        format!("@{me}")
    } else {
        group.to_string()
    }
}

fn text_of(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => serde_json::to_string_pretty(&other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_only_tool, web_tool, Server};
    use crate::config::Config;
    use crate::engine::Engine;
    use crate::store::{Agent, Store};
    use serde_json::json;

    #[test]
    fn records_what_agents_do() {
        let dir = std::env::temp_dir().join(format!("drovr-mcp-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = dir.join("plans").display().to_string();
        cfg.defaults.auto_wake = false;
        // Off, agents don't get record_decision.
        let names = |cfg: &Config| -> Vec<String> {
            super::tools(cfg, false)
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(!names(&cfg).contains(&"record_decision".to_string()));
        cfg.workflow.enabled = true;
        assert!(names(&cfg).contains(&"record_decision".to_string()));
        // Handoffs and checkpoints only in a group that works in stages.
        assert!(!names(&cfg).contains(&"handoff".to_string()));
        let staged: Vec<String> = super::tools(&cfg, true)
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(staged.contains(&"handoff".to_string()));
        let store = Store::open(&dir.join("t.db")).unwrap();
        for (name, ticket) in [("api", "SRE-7"), ("web", "")] {
            store
                .add_agent(&Agent {
                    name: name.into(),
                    role: "solo".into(),
                    ticket: ticket.into(),
                    ..Agent::default()
                })
                .unwrap();
        }
        let engine = Engine::with(store, cfg.clone(), "drovr".into());
        let api = Server::new(engine, "api".into());
        api.call("set_status", &json!({ "status": "reading the module" }))
            .unwrap();
        api.call(
            "send_message",
            &json!({ "to": "web", "body": "token ghp_abcdefghijklmnopqrstuvwxyz0123" }),
        )
        .unwrap();
        let r = api
            .call("record_decision", &json!({ "question": "Which lib?", "options": ["a", "b"], "chosen": "b", "rationale": "smaller" }))
            .unwrap();
        assert_eq!(r, json!("recorded"));
        assert!(api
            .call("record_decision", &json!({ "question": "" }))
            .is_err());
        // Reading doesn't add to the record.
        api.call("whoami", &json!({})).unwrap();

        let t = crate::ledger::timeline(&cfg, "SRE-7");
        let kinds: Vec<&str> = t.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["status", "message", "decision"]);
        assert_eq!(t[1]["body"], "token ghp_[redacted]");
        assert_eq!(
            (t[2]["chosen"].as_str(), t[2]["by"].as_str()),
            (Some("b"), Some("agent"))
        );

        // An agent with no ticket is told there's nowhere to record it.
        let web = Server::new(
            Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into()),
            "web".into(),
        );
        let r = web
            .call(
                "record_decision",
                &json!({ "question": "q", "chosen": "c" }),
            )
            .unwrap();
        assert!(r.as_str().unwrap().contains("no ticket"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spots_web_tools() {
        for t in [
            "mcp__fetch__fetch",
            "mcp__browser__get_page_text",
            "mcp__playwright__browser_navigate",
            "mcp__firecrawl__firecrawl_scrape",
            "mcp__tools__fetch_url",
            "mcp__brave__brave_web_search",
        ] {
            assert!(web_tool(t), "{t} reaches the web");
        }
        for t in [
            "mcp__atlassian__getJiraIssue",
            "mcp__claude_ai_Atlassian__searchJiraIssuesUsingJql",
            "mcp__slack__slack_read_channel",
            "mcp__confluence__fetchConfluencePage",
            "Read",
        ] {
            assert!(!web_tool(t), "{t} doesn't reach the web");
        }
    }

    #[test]
    fn classifies_read_only_tools() {
        for t in [
            "mcp__atlassian__searchJiraIssuesUsingJql",
            "mcp__atlassian__getAccessibleAtlassianResources",
            "mcp__claude_ai_Atlassian__getJiraIssue",
            "mcp__atlassian__lookupJiraAccountId",
            "mcp__slack__slack_search_public",
            "mcp__slack__slack_read_channel",
            "mcp__confluence__getConfluencePage",
        ] {
            assert!(read_only_tool(t), "{t} should be read-only");
        }
        for t in [
            "mcp__atlassian__createJiraIssue",
            "mcp__atlassian__transitionJiraIssue",
            "mcp__atlassian__addCommentToJiraIssue",
            "mcp__slack__slack_send_message",
            "mcp__atlassian__editJiraIssue",
            "Bash",
            "Write",
            "mcp__atlassian__doSomething",
        ] {
            assert!(!read_only_tool(t), "{t} should not be read-only");
        }
    }
}
