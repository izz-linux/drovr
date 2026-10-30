//! The retro, part of the ticket workflow, run only when you start one.
//!
//! drovr works out the numbers from the tickets' records itself (cost and
//! tokens by stage and model, rework, dismissals, misses, overrides,
//! wakes) and writes them to `evidence.json`. A retro agent reads the
//! records and those numbers, and submits proposals with evidence:
//! a model per stage, thresholds, wake rules, process, skill edits and new
//! skills. You approve them one by one or all at once; approving one
//! applies it (to your config, keeping its comments, or to
//! `~/.claude/skills`), and every decision goes back into the records.
//! Each retro lives in `<plans>/retros/<when>/`.

use crate::engine::{Engine, NewAgent};
use crate::ledger;
use crate::store::Agent;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a proposal may be.
pub const KINDS: &[&str] = &[
    "model",
    "threshold",
    "wake_rule",
    "process",
    "skill_edit",
    "new_skill",
];

/// Config keys an approved proposal may change. Anything else is yours to
/// change by hand.
const KEYS: &[&str] = &[
    "stages.models.",
    "stages.order",
    "stages.build_checkpoint_tokens",
    "stages.build_checkpoint_usd",
    "stages.tidy_on_close",
    "watch.rules.",
    "watch.merged_transition",
    "retro.model",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Proposal {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub why: String,
    pub evidence: Vec<String>,
    /// `{"config": {"key": …, "value": …}}`, `{"skill": {"name": …,
    /// "content": …}}`, or `{}` for a change you make yourself.
    pub change: Value,
    /// `pending`, `approved` or `dismissed`.
    pub status: String,
    /// What approving it did.
    pub result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Retro {
    pub dir: String,
    pub tickets: Vec<String>,
    pub started: f64,
    pub agent: String,
    pub note: String,
    pub proposals: Vec<Proposal>,
}

fn retros_dir(cfg: &crate::config::Config) -> PathBuf {
    cfg.reports.path().join("retros")
}

/// The numbers a retro starts from, worked out from the records so the
/// agent doesn't have to count.
pub fn evidence(cfg: &crate::config::Config, tickets: &[String]) -> Value {
    let mut per_ticket = Map::new();
    #[derive(Default)]
    struct Row {
        runs: u64,
        cost: f64,
        tokens_in: u64,
        tokens_out: u64,
        cache_read: u64,
    }
    let mut by_stage_model: BTreeMap<(String, String), Row> = BTreeMap::new();
    for t in tickets {
        let events = ledger::timeline(cfg, t);
        let mut stage_of: BTreeMap<String, String> = BTreeMap::new();
        let mut model_of: BTreeMap<(String, i64), String> = BTreeMap::new();
        let mut n: BTreeMap<&str, i64> = BTreeMap::new();
        let mut bump = |k: &'static str| *n.entry(k).or_default() += 1;
        let (mut cost, mut waits) = (0.0, vec![]);
        let mut next_model: BTreeMap<i64, String> = BTreeMap::new();
        for e in &events {
            let agent = e["agent"].as_str().unwrap_or("").to_string();
            match e["kind"].as_str().unwrap_or("") {
                "agent_started" => {
                    stage_of.insert(agent, e["stage"].as_str().unwrap_or("").to_string());
                }
                "run_start" => {
                    model_of.insert(
                        (agent, e["run"].as_i64().unwrap_or(0)),
                        e["model"].as_str().unwrap_or("").to_string(),
                    );
                }
                "run_end" => {
                    bump("runs");
                    let c = e["cost_usd"].as_f64().unwrap_or(0.0);
                    cost += c;
                    let model = e["models"][0]
                        .as_str()
                        .map(String::from)
                        .or_else(|| {
                            model_of
                                .get(&(agent.clone(), e["run"].as_i64().unwrap_or(0)))
                                .cloned()
                        })
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| "default".into());
                    let stage = stage_of
                        .get(&agent)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "none".into());
                    let row = by_stage_model.entry((stage, model)).or_default();
                    let tok = |k: &str| e["tokens"][k].as_u64().unwrap_or(0);
                    row.runs += 1;
                    row.cost += c;
                    row.tokens_in += tok("input");
                    row.tokens_out += tok("output");
                    row.cache_read += tok("cache_read");
                    if e["state"] == "error" {
                        bump("runs_failed");
                    }
                }
                "asked" => {
                    bump("asked");
                    if e["type"] == "handoff" {
                        if let Some(m) = e["payload"]["next_model"].as_str() {
                            next_model.insert(e["item"].as_i64().unwrap_or(0), m.to_string());
                        }
                    }
                }
                "answered" => {
                    match e["status"].as_str() {
                        Some("rejected") => bump("declined"),
                        Some("dismissed") => bump("dismissed"),
                        _ => bump("answered"),
                    }
                    if e["type"] == "decision" {
                        bump("decisions_yours");
                    }
                    if let Some(w) = e["waited_s"].as_f64() {
                        waits.push(w);
                    }
                }
                "decision" => bump("decisions_agents"),
                "handoff" => bump("handoffs"),
                "handoff_sent_back" => bump("rework_handoffs_sent_back"),
                "handoff_approved" => {
                    // You chose a different model for the next stage.
                    let chosen = e["model"].as_str().unwrap_or("");
                    if !chosen.is_empty() && !next_model.values().any(|m| m == chosen) {
                        bump("overrides_model");
                    }
                }
                "pr_status" => {
                    if e["changes"]["ci"][1] == "failure" {
                        bump("rework_ci_failures");
                    }
                    if e["changes"]["review"][1] == "CHANGES_REQUESTED" {
                        bump("rework_changes_requested");
                    }
                    if e["changes"]["mergeable"][1] == "CONFLICTING" {
                        bump("conflicts");
                    }
                }
                "pr_merged" => bump("prs_merged"),
                "pr_closed" => bump("misses_prs_closed_unmerged"),
                "wake" => bump("wakes"),
                "wake_paused" => bump("wakes_paused"),
                "checkpoint" => bump("checkpoints"),
                "stopped" if e["by"] == "you" => bump("stopped_by_you"),
                "send_now" => bump("interrupted_by_you"),
                _ => {}
            }
        }
        let span = match (events.first(), events.last()) {
            (Some(a), Some(b)) => {
                (b["ts"].as_f64().unwrap_or(0.0) - a["ts"].as_f64().unwrap_or(0.0)) / 3600.0
            }
            _ => 0.0,
        };
        let mut counts: Map<String, Value> = n
            .into_iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        counts.insert("cost_usd".into(), json!((cost * 100.0).round() / 100.0));
        counts.insert("hours".into(), json!((span * 10.0).round() / 10.0));
        counts.insert("events".into(), json!(events.len()));
        if !waits.is_empty() {
            counts.insert(
                "avg_inbox_wait_s".into(),
                json!((waits.iter().sum::<f64>() / waits.len() as f64).round()),
            );
        }
        per_ticket.insert(t.clone(), Value::Object(counts));
    }
    let rows: Vec<Value> = by_stage_model
        .into_iter()
        .map(|((stage, model), r)| {
            json!({ "stage": stage, "model": model, "runs": r.runs, "cost_usd": (r.cost * 100.0).round() / 100.0,
                    "tokens_in": r.tokens_in, "tokens_out": r.tokens_out, "cache_read": r.cache_read })
        })
        .collect();
    json!({ "tickets": per_ticket, "by_stage_and_model": rows })
}

/// The skills you have, by name.
fn skills(cfg: &crate::config::Config) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(crate::config::expand_tilde(&cfg.retro.skills_dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("SKILL.md").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
    names.sort();
    names
}

impl Engine {
    /// Start a retro on `tickets`: work out the evidence, then start the
    /// retro agent on it.
    pub fn start_retro(&self, tickets: &[String], note: &str) -> Result<Agent> {
        if !self.cfg.workflow.enabled {
            bail!("the ticket workflow is off (workflow.enabled)");
        }
        let tickets: Vec<String> = tickets.iter().flat_map(|t| ledger::tickets_in(t)).collect();
        let records: Vec<ledger::Record> = ledger::list(&self.cfg)
            .into_iter()
            .filter(|r| tickets.contains(&r.ticket))
            .collect();
        if tickets.is_empty() || records.is_empty() {
            bail!("pick tickets that have a record");
        }
        let now = crate::store::now();
        let stamp = ledger::iso(now)
            .replace([':', '-'], "")
            .replace('T', "-")
            .trim_end_matches('Z')
            .to_string();
        let dir = retros_dir(&self.cfg).join(&stamp);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("evidence.json"),
            serde_json::to_string_pretty(&evidence(&self.cfg, &tickets))?,
        )?;
        let name = format!("retro-{}", stamp[..13].replace('-', ""));
        let meta = Retro {
            dir: dir.display().to_string(),
            tickets: tickets.clone(),
            started: now,
            agent: name.clone(),
            note: note.to_string(),
            proposals: vec![],
        };
        std::fs::write(dir.join("retro.json"), serde_json::to_string_pretty(&meta)?)?;
        let setup = json!({
            "stages": { "order": self.cfg.stages.order, "models": self.cfg.stages.models,
                        "build_checkpoint_tokens": self.cfg.stages.build_checkpoint_tokens, "build_checkpoint_usd": self.cfg.stages.build_checkpoint_usd },
            "watch.rules": self.cfg.watch.rules,
            "default_model": self.cfg.defaults.model,
        });
        let folders: Vec<String> = records.iter().map(|r| format!("- {}", r.path)).collect();
        let you = self.cfg.your_name();
        let task = format!(
            r#"Run a retro on {tickets} for {you}, to make the next tickets go better and cost less.

Everything that happened is in these folders: `ledger.jsonl` (one JSON event per line: runs with model, tokens and cost; questions and answers; decisions; handoffs; wakes; PR and Jira changes), with `handoffs/` and `checkpoints/` beside it.
{folders}

drovr has already worked out the numbers from them: {dir}/evidence.json. Use those rather than recounting.

The current setup: {setup}
Skills in {skills_dir}: {skills}
{note}
Look for rework (handoffs sent back, CI failures, changes requested), dismissals (Inbox items declined or dismissed, drafts discarded), misses (problems found late, PRs closed unmerged, decisions reversed), overrides (a stage's model changed at a handoff), and cost by stage and model. Then call `submit_proposals` with what you'd change, each with its evidence:
- model: a stage's model, as {{"config": {{"key": "stages.models.<stage>", "value": "<model>"}}}}. A cheaper model where rework stays low, a stronger one where rework costs more than it saves.
- threshold: {{"config": {{"key": "stages.build_checkpoint_tokens" or "stages.build_checkpoint_usd", "value": <number>}}}}
- wake_rule: {{"config": {{"key": "watch.rules.<rule>", "value": "wake" | "ask" | "ignore"}}}}
- process: a setting when it maps to one (e.g. stages.order), else {{}} with the change in why.
- skill_edit: {{"skill": {{"name": "<an existing skill>", "content": "<the whole new SKILL.md>"}}}}
- new_skill: {{"skill": {{"name": "<new-skill-name>", "content": "<a whole SKILL.md, with name and description front matter>"}}}}
Propose only what the evidence supports; proposing nothing is fine. {you} approves each one.

Then publish a short report with `publish_report` (title "Retro: {tickets}"): what went well, what didn't, and your proposals. Then `report_done`."#,
            tickets = tickets.join(", "),
            folders = folders.join("\n"),
            dir = dir.display(),
            setup = setup,
            skills_dir = self.cfg.retro.skills_dir,
            skills = if skills(&self.cfg).is_empty() {
                "none".to_string()
            } else {
                skills(&self.cfg).join(", ")
            },
            note = if note.trim().is_empty() {
                String::new()
            } else {
                format!("\n{you}'s note: {}\n", note.trim())
            },
        );
        let agent = self.start_agent(NewAgent {
            name,
            role: "solo".into(),
            cwd: dir.display().to_string(),
            task,
            stage: "retro".into(),
            model: self.cfg.retro.model.clone(),
            ..Default::default()
        })?;
        for t in &tickets {
            self.record_ticket(
                t,
                "retro_started",
                json!({ "dir": dir, "agent": agent.name, "tickets": tickets }),
            );
        }
        Ok(agent)
    }

    /// The retro agent submits its proposals.
    pub fn submit_proposals(&self, me: &Agent, a: &Value) -> Result<Value> {
        if me.stage != "retro" {
            bail!("only a retro's agent submits proposals");
        }
        let dir = PathBuf::from(&me.cwd);
        if !dir.starts_with(retros_dir(&self.cfg)) || !dir.join("retro.json").is_file() {
            bail!("your working directory isn't a retro's folder");
        }
        let list = a["proposals"]
            .as_array()
            .ok_or_else(|| anyhow!("proposals must be a list"))?;
        let mut out = vec![];
        for (i, p) in list.iter().enumerate() {
            let mut p: Proposal = serde_json::from_value(p.clone())
                .map_err(|e| anyhow!("proposal {}: {e}", i + 1))?;
            if !KINDS.contains(&p.kind.as_str()) {
                bail!(
                    "proposal {}: kind must be one of {}",
                    i + 1,
                    KINDS.join(", ")
                );
            }
            if p.title.trim().is_empty() || p.evidence.is_empty() {
                bail!("proposal {}: a title and evidence are required", i + 1);
            }
            check_change(&p.change).map_err(|e| anyhow!("proposal {}: {e}", i + 1))?;
            p.id = format!("p{}", i + 1);
            p.status = "pending".into();
            p.result.clear();
            out.push(p);
        }
        std::fs::write(
            dir.join("proposals.json"),
            ledger::redact(&serde_json::to_string_pretty(&out)?),
        )?;
        let meta = read_meta(&dir)?;
        for t in &meta.tickets {
            self.record_ticket(
                t,
                "retro_proposals",
                json!({ "dir": dir, "count": out.len() }),
            );
        }
        Ok(
            json!({ "saved": out.len(), "note": format!("{} reviews them in drovr's Pipeline view.", crate::engine::capitalize(&self.cfg.your_name())) }),
        )
    }

    /// Every retro, newest first, with its proposals and what you decided.
    pub fn retros(&self) -> Vec<Retro> {
        let mut out: Vec<Retro> = std::fs::read_dir(retros_dir(&self.cfg))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let mut r = read_meta(&e.path()).ok()?;
                r.proposals = proposals(&e.path());
                Some(r)
            })
            .collect();
        out.sort_by(|a, b| b.started.total_cmp(&a.started));
        out
    }

    /// Approve (and apply) or dismiss a proposal.
    pub fn decide_proposal(&self, dir: &str, id: &str, approve: bool) -> Result<String> {
        let dir = PathBuf::from(dir);
        if !dir.starts_with(retros_dir(&self.cfg)) {
            bail!("{} isn't a retro", dir.display());
        }
        let meta = read_meta(&dir)?;
        let p = proposals(&dir)
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow!("no proposal {id}"))?;
        if p.status != "pending" {
            bail!("proposal {id} was already {}", p.status);
        }
        let result = if approve {
            self.apply(&p.change)?
        } else {
            String::new()
        };
        let status = if approve { "approved" } else { "dismissed" };
        let line =
            json!({ "id": id, "status": status, "result": result, "ts": crate::store::now() });
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("decisions.jsonl"))?
            .write_all(format!("{line}\n").as_bytes())?;
        for t in &meta.tickets {
            self.record_ticket(
                t,
                &format!("proposal_{status}"),
                json!({ "id": id, "proposal_kind": p.kind, "title": p.title, "result": result, "by": "you" }),
            );
        }
        Ok(result)
    }

    fn apply(&self, change: &Value) -> Result<String> {
        if let Some(c) = change.get("config") {
            let key = c["key"].as_str().unwrap_or("");
            return set_config(key, &c["value"]);
        }
        if let Some(s) = change.get("skill") {
            let dir = crate::config::expand_tilde(&self.cfg.retro.skills_dir);
            return write_skill(
                Path::new(&dir),
                s["name"].as_str().unwrap_or(""),
                s["content"].as_str().unwrap_or(""),
            );
        }
        Ok("Noted; this one is yours to carry out.".into())
    }
}

fn read_meta(dir: &Path) -> Result<Retro> {
    let text = std::fs::read_to_string(dir.join("retro.json"))?;
    let mut r: Retro = serde_json::from_str(&text)?;
    r.dir = dir.display().to_string();
    Ok(r)
}

/// A retro's proposals, with what you decided about each.
fn proposals(dir: &Path) -> Vec<Proposal> {
    let mut list: Vec<Proposal> = std::fs::read_to_string(dir.join("proposals.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for line in std::fs::read_to_string(dir.join("decisions.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        let Ok(d) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(p) = list.iter_mut().find(|p| d["id"] == p.id.as_str()) {
            p.status = d["status"].as_str().unwrap_or("").to_string();
            p.result = d["result"].as_str().unwrap_or("").to_string();
        }
    }
    list
}

fn check_change(change: &Value) -> Result<()> {
    if let Some(c) = change.get("config") {
        let key = c["key"].as_str().unwrap_or("");
        let allowed = KEYS.iter().any(|k| match k.strip_suffix('.') {
            // A table: any key in it.
            Some(_) => key.starts_with(k) && key.len() > k.len(),
            None => key == *k,
        });
        if !allowed {
            bail!("drovr can only change {} (not {key:?})", KEYS.join(", "));
        }
        if c["value"].is_null() || c["value"].is_object() {
            bail!("config value must be a string, number, true/false or a list");
        }
    } else if let Some(s) = change.get("skill") {
        skill_name(s["name"].as_str().unwrap_or(""))?;
        if s["content"].as_str().unwrap_or("").trim().is_empty() {
            bail!("a skill needs its whole SKILL.md as content");
        }
    }
    Ok(())
}

fn skill_name(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if !ok {
        bail!("skill names are lowercase letters, digits and dashes (got {name:?})");
    }
    Ok(name)
}

/// Set `key` (dotted) in your config file, keeping its comments and layout.
pub fn set_config(key: &str, value: &Value) -> Result<String> {
    check_change(&json!({ "config": { "key": key, "value": value } }))?;
    let path = crate::config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = set_in(&text, key, value)?;
    crate::config::parse(&new)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !text.is_empty() {
        std::fs::write(path.with_extension("toml.before-retro"), &text)?;
    }
    std::fs::write(&path, new)?;
    Ok(format!("Set {key} = {value} in {}", path.display()))
}

/// `text` with `key` set to `value`.
pub fn set_in(text: &str, key: &str, value: &Value) -> Result<String> {
    let mut doc: toml_edit::Document = text.parse()?;
    let parts: Vec<&str> = key.split('.').collect();
    let (last, tables) = parts.split_last().ok_or_else(|| anyhow!("empty key"))?;
    let mut item = doc.as_item_mut();
    for t in tables {
        if item.get(t).is_none() {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            item[t] = toml_edit::Item::Table(table);
        }
        item = &mut item[t];
    }
    item[last] = toml_edit::value(to_toml(value)?);
    Ok(doc.to_string())
}

fn to_toml(v: &Value) -> Result<toml_edit::Value> {
    Ok(match v {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or(0.0).into(),
        },
        Value::Array(items) => {
            let mut a = toml_edit::Array::new();
            for i in items {
                a.push(to_toml(i)?);
            }
            a.into()
        }
        _ => bail!("can't put {v} in the config"),
    })
}

/// Write a skill's SKILL.md, keeping the one it replaces beside it.
fn write_skill(dir: &Path, name: &str, content: &str) -> Result<String> {
    let name = skill_name(name)?;
    let skill = dir.join(name);
    std::fs::create_dir_all(&skill)?;
    let file = skill.join("SKILL.md");
    let note = if file.is_file() {
        let backup = skill.join(format!(
            "SKILL.md.before-retro-{}",
            crate::store::now() as i64
        ));
        std::fs::rename(&file, &backup)?;
        format!(" (the old one is {})", backup.display())
    } else {
        String::new()
    };
    std::fs::write(&file, content)?;
    Ok(format!("Wrote {}{note}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_config_keeping_comments() {
        let text =
            "# my config\n[defaults]\nmodel = \"\" # keep me\n\n[stages.models]\ntriage = \"x\"\n";
        let out = set_in(text, "stages.models.triage", &json!("claude-haiku-5-5")).unwrap();
        assert!(out.contains("# my config") && out.contains("# keep me"));
        assert!(out.contains("triage = \"claude-haiku-5-5\""));
        let out = set_in(&out, "watch.rules.ci_failed", &json!("ask")).unwrap();
        let cfg = crate::config::parse(&out).unwrap();
        assert_eq!(
            (
                cfg.watch.rules.ci_failed.as_str(),
                cfg.stages.model("triage")
            ),
            ("ask", "claude-haiku-5-5")
        );
        let out = set_in(&out, "stages.build_checkpoint_tokens", &json!(90000)).unwrap();
        assert_eq!(
            crate::config::parse(&out)
                .unwrap()
                .stages
                .build_checkpoint_tokens,
            90000
        );
        let out = set_in("", "stages.order", &json!(["triage", "build"])).unwrap();
        assert_eq!(
            crate::config::parse(&out).unwrap().stages.order,
            ["triage", "build"]
        );
    }

    #[test]
    fn only_some_changes_are_allowed() {
        assert!(
            check_change(&json!({ "config": { "key": "stages.models.build", "value": "m" } }))
                .is_ok()
        );
        assert!(check_change(
            &json!({ "config": { "key": "watch.rules.conflict", "value": "ask" } })
        )
        .is_ok());
        assert!(check_change(
            &json!({ "config": { "key": "claude.command", "value": "rm -rf /" } })
        )
        .is_err());
        assert!(
            check_change(&json!({ "config": { "key": "stages.models.", "value": "m" } })).is_err()
        );
        assert!(check_change(&json!({ "skill": { "name": "../evil", "content": "x" } })).is_err());
        assert!(
            check_change(&json!({ "skill": { "name": "release-risk", "content": "" } })).is_err()
        );
        assert!(check_change(&json!({})).is_ok());
    }

    #[test]
    fn writes_skills_and_keeps_the_old_one() {
        let dir = std::env::temp_dir().join(format!("drovr-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_skill(&dir, "release-risk", "v1").unwrap();
        let r = write_skill(&dir, "release-risk", "v2").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("release-risk/SKILL.md")).unwrap(),
            "v2"
        );
        assert!(r.contains("before-retro"));
        assert_eq!(
            std::fs::read_dir(dir.join("release-risk")).unwrap().count(),
            2
        );
        assert!(write_skill(&dir, "Bad Name", "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn takes_proposals_and_your_decisions() {
        let dir = std::env::temp_dir().join(format!("drovr-retro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.workflow.enabled = true;
        cfg.reports.dir = dir.join("plans").display().to_string();
        let engine = Engine::with(
            crate::store::Store::open(&dir.join("t.db")).unwrap(),
            cfg,
            "drovr".into(),
        );
        engine
            .store
            .add_group("g", "", "SRE-1", "/tmp", "", false)
            .unwrap();
        ledger::append(&engine.cfg, "g", "SRE-1", &json!({ "kind": "status" })).unwrap();
        // A retro's folder, as start_retro leaves it.
        let rdir = retros_dir(&engine.cfg).join("20261029-0300");
        std::fs::create_dir_all(&rdir).unwrap();
        let meta = Retro {
            tickets: vec!["SRE-1".into()],
            started: 1.0,
            agent: "retro-x".into(),
            ..Retro::default()
        };
        std::fs::write(
            rdir.join("retro.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let me = Agent {
            name: "retro-x".into(),
            stage: "retro".into(),
            cwd: rdir.display().to_string(),
            ..Agent::default()
        };

        // Proposals are checked before they're kept.
        let bad = json!({ "proposals": [{ "kind": "model", "title": "t", "evidence": ["e"], "change": { "config": { "key": "claude.command", "value": "x" } } }] });
        assert!(engine.submit_proposals(&me, &bad).is_err());
        let other = Agent {
            stage: "build".into(),
            ..me.clone()
        };
        assert!(engine
            .submit_proposals(&other, &json!({ "proposals": [] }))
            .is_err());
        let good = json!({ "proposals": [
            { "kind": "model", "title": "Triage on haiku", "why": "No rework at half the cost", "evidence": ["SRE-1: 0 sent back"], "change": { "config": { "key": "stages.models.triage", "value": "claude-haiku-5-5" } } },
            { "kind": "process", "title": "Pair research with blueprint", "evidence": ["SRE-1: blueprint redid research"], "change": {} }
        ] });
        assert_eq!(engine.submit_proposals(&me, &good).unwrap()["saved"], 2);

        let r = engine.retros();
        assert_eq!(r.len(), 1);
        assert_eq!(
            (
                r[0].proposals[0].id.as_str(),
                r[0].proposals[0].status.as_str()
            ),
            ("p1", "pending")
        );
        // A change you make yourself; then a dismissal.
        let done = engine.decide_proposal(&r[0].dir, "p2", true).unwrap();
        assert!(done.contains("yours to carry out"));
        engine.decide_proposal(&r[0].dir, "p1", false).unwrap();
        assert!(engine.decide_proposal(&r[0].dir, "p1", true).is_err());
        let r = engine.retros();
        let st: Vec<&str> = r[0].proposals.iter().map(|p| p.status.as_str()).collect();
        assert_eq!(st, ["dismissed", "approved"]);
        assert!(engine
            .decide_proposal(dir.to_str().unwrap(), "p1", true)
            .is_err());
        let kinds: Vec<String> = ledger::timeline(&engine.cfg, "SRE-1")
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "status",
                "retro_proposals",
                "proposal_approved",
                "proposal_dismissed"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn works_out_the_evidence() {
        let dir = std::env::temp_dir().join(format!("drovr-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = crate::config::Config::default();
        cfg.reports.dir = dir.display().to_string();
        let add = |e: Value| ledger::append(&cfg, "g", "SRE-1", &e).unwrap();
        add(json!({ "kind": "agent_started", "agent": "g-triage", "stage": "triage" }));
        add(json!({ "kind": "run_start", "agent": "g-triage", "run": 1, "model": "" }));
        add(
            json!({ "kind": "run_end", "agent": "g-triage", "run": 1, "cost_usd": 0.5, "models": ["claude-sonnet-5-5"], "tokens": { "input": 10, "output": 20 } }),
        );
        add(json!({ "kind": "handoff_sent_back", "agent": "g-triage" }));
        add(
            json!({ "kind": "asked", "agent": "g-triage", "type": "handoff", "item": 3, "payload": { "next_model": "" } }),
        );
        add(json!({ "kind": "handoff_approved", "agent": "g-triage", "model": "claude-opus-5-5" }));
        add(
            json!({ "kind": "answered", "agent": "g-triage", "type": "jira", "status": "dismissed", "waited_s": 40 }),
        );
        add(json!({ "kind": "pr_status", "changes": { "ci": ["pending", "failure"] } }));
        let e = evidence(&cfg, &["SRE-1".into()]);
        let t = &e["tickets"]["SRE-1"];
        assert_eq!(
            (
                t["runs"].as_i64(),
                t["rework_handoffs_sent_back"].as_i64(),
                t["dismissed"].as_i64()
            ),
            (Some(1), Some(1), Some(1))
        );
        assert_eq!(
            (
                t["rework_ci_failures"].as_i64(),
                t["overrides_model"].as_i64(),
                t["cost_usd"].as_f64()
            ),
            (Some(1), Some(1), Some(0.5))
        );
        assert_eq!(
            e["by_stage_and_model"][0],
            json!({ "stage": "triage", "model": "claude-sonnet-5-5", "runs": 1, "cost_usd": 0.5, "tokens_in": 10, "tokens_out": 20, "cache_read": 0 })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
