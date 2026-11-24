//! The spend log: what every run cost, by model, for Insights. It's kept
//! whether or not the ticket workflow is on, and holds figures and names
//! only: never a prompt, a message or a task. Runs from before it existed
//! are read from the tickets' records, once.

use crate::config::Config;
use crate::engine::Engine;
use crate::store::Agent;
use crate::{harness, ledger, models};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// What one model cost in one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub ts: f64,
    pub agent: String,
    pub role: String,
    pub group_name: String,
    /// Ticket keys, comma-separated.
    pub tickets: String,
    pub stage: String,
    pub harness: String,
    /// The model as configured; empty for the harness's default.
    pub started_as: String,
    /// Its name for reporting (`models::canonical`).
    pub model: String,
    pub run: i64,
    /// The run's main model, whose row carries its tokens and time.
    pub main: bool,
    /// None when the CLI didn't report a cost.
    pub cost_usd: Option<f64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub duration_ms: Option<i64>,
    pub state: String,
    /// `run`, or `import` for one read from a ticket's record.
    pub source: String,
    /// Keeps the same run from being added twice.
    pub key: String,
}

/// A row per model for a run that ended with `end` (a `run_end` event's
/// fields), from what `run` says about it. `run.key` is the run's own.
fn rows(run: &Row, priced: bool, end: &Value) -> Vec<Row> {
    let (main, split) = models::split(end, &run.started_as);
    let tokens = |k: &str| end["tokens"][k].as_i64();
    split
        .into_iter()
        .map(|(model, usd)| {
            let mut row = Row {
                key: format!("{}:{model}", run.key),
                cost_usd: priced.then_some(usd),
                state: end["state"].as_str().unwrap_or("").into(),
                ..run.clone()
            };
            if model == main {
                row.main = true;
                row.tokens_in = tokens("input");
                row.tokens_out = tokens("output");
                row.cache_read = tokens("cache_read");
                row.cache_write = tokens("cache_write");
                row.duration_ms = end["duration_ms"].as_f64().map(|ms| ms as i64);
            }
            row.model = model;
            row
        })
        .collect()
}

impl Engine {
    /// Add a run that just ended to the log. A failure is only printed: it
    /// must not stop the run from finishing.
    pub(crate) fn log_spend(&self, agent: &Agent, run: i64, priced: bool, end: &Value) {
        let mut tickets = ledger::tickets_in(&agent.ticket);
        if tickets.is_empty() {
            if let Ok(Some(g)) = self.store.group(&agent.group_name) {
                tickets = ledger::tickets_in(&g.ticket);
            }
        }
        let harness = harness::of(agent);
        let run = Row {
            ts: crate::store::now(),
            agent: agent.name.clone(),
            role: agent.role.clone(),
            group_name: agent.group_name.clone(),
            tickets: tickets.join(","),
            stage: agent.stage.clone(),
            harness: harness.name().into(),
            started_as: harness::model_of(harness, agent, &self.cfg),
            run,
            source: "run".into(),
            key: format!("{}:{}:{run}", agent.name, agent.created),
            ..Row::default()
        };
        if let Err(e) = self.store.add_spend(&rows(&run, priced, end)) {
            eprintln!("drovr: couldn't log what {}'s run cost: {e:#}", agent.name);
        }
    }

    /// Add the runs in the tickets' records from before the log existed.
    /// Once per database; returns how many rows it added.
    pub fn import_spend(&self) -> Result<usize> {
        const ONCE: &str = "import spend";
        if self.store.done_once(ONCE)? {
            return Ok(0);
        }
        // A run on several tickets is in each one's record, a moment apart.
        let mut runs: BTreeMap<(String, i64), Vec<(Row, Value)>> = BTreeMap::new();
        for (ticket, events) in records(&self.cfg) {
            for (row, end) in recorded(&ticket, &events) {
                let same = runs.entry((row.agent.clone(), row.run)).or_default();
                same.push((row, end));
            }
        }
        let mut out = vec![];
        for mut same in runs.into_values() {
            same.sort_by(|a, b| a.0.ts.total_cmp(&b.0.ts));
            let mut merged: Vec<(Row, Value, BTreeSet<String>)> = vec![];
            for (row, end) in same {
                match merged.last_mut() {
                    Some((first, _, tickets)) if row.ts - first.ts < 2.0 => {
                        tickets.insert(row.tickets);
                    }
                    _ => {
                        let tickets = BTreeSet::from([row.tickets.clone()]);
                        merged.push((row, end, tickets));
                    }
                }
            }
            for (mut row, end, tickets) in merged {
                row.tickets = tickets.into_iter().collect::<Vec<_>>().join(",");
                row.key = format!("import:{}:{}:{}", row.agent, row.run, row.ts);
                // Records don't say whether the CLI reported a cost.
                let priced = end["cost_usd"].as_f64().unwrap_or(0.0) > 0.0;
                out.extend(rows(&row, priced, &end));
            }
        }
        self.store.import_spend(ONCE, &out)
    }
}

/// Every ticket's record, archived ones too.
pub(crate) fn records(cfg: &Config) -> Vec<(String, Vec<Value>)> {
    let plans = cfg.reports.path();
    let mut out = vec![];
    for root in [plans.join(ledger::ARCHIVE), plans] {
        for entry in subdirs(&root) {
            if entry.ends_with(ledger::ARCHIVE) {
                continue;
            }
            for dir in std::iter::once(entry.clone()).chain(subdirs(&entry)) {
                if ledger::is_record(&dir) {
                    let name = dir.file_name().unwrap_or_default().to_string_lossy();
                    out.push((name.to_string(), ledger::read(&dir)));
                }
            }
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The runs in `events`, which are one ticket's, oldest first.
fn recorded(ticket: &str, events: &[Value]) -> Vec<(Row, Value)> {
    let text = |e: &Value, k: &str| e[k].as_str().unwrap_or("").to_string();
    let mut stages: BTreeMap<String, String> = BTreeMap::new();
    let mut started: BTreeMap<(String, i64), (String, String)> = BTreeMap::new();
    let mut out = vec![];
    for e in events {
        let agent = text(e, "agent");
        let run = e["run"].as_i64().unwrap_or(0);
        match e["kind"].as_str().unwrap_or("") {
            "agent_started" => {
                stages.insert(agent, text(e, "stage"));
            }
            "run_start" => {
                started.insert((agent, run), (text(e, "harness"), text(e, "model")));
            }
            "run_end" => {
                let (harness, started_as) =
                    started.remove(&(agent.clone(), run)).unwrap_or_default();
                let row = Row {
                    ts: e["ts"].as_f64().unwrap_or(0.0),
                    role: text(e, "role"),
                    group_name: text(e, "group"),
                    tickets: e["ticket"].as_str().unwrap_or(ticket).into(),
                    stage: stages.get(&agent).cloned().unwrap_or_default(),
                    harness,
                    started_as,
                    agent,
                    run,
                    source: "import".into(),
                    ..Row::default()
                };
                out.push((row, e.clone()));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use serde_json::json;

    fn engine(case: &str, workflow: bool) -> (Engine, PathBuf) {
        let dir = std::env::temp_dir().join(format!("drovr-spend-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.workflow.enabled = workflow;
        cfg.reports.dir = dir.join("plans").display().to_string();
        let store = Store::open(&dir.join("t.db")).unwrap();
        (Engine::with(store, cfg, "drovr".into()), dir)
    }

    fn agent(name: &str, created: f64) -> Agent {
        Agent {
            name: name.into(),
            role: "worker".into(),
            group_name: "g".into(),
            harness: "claude".into(),
            stage: "build".into(),
            model: "opus".into(),
            ticket: "SRE-1, SRE-2".into(),
            task: "a secret task".into(),
            created,
            ..Agent::default()
        }
    }

    fn end() -> Value {
        json!({
            "state": "idle", "cost_usd": 1.25, "duration_ms": 900,
            "tokens": { "input": 10, "output": 20, "cache_read": 30, "cache_write": 40 },
            "models": ["claude-haiku-5-5", "claude-opus-5-5"],
            "model_costs": { "claude-haiku-5-5": 0.25, "claude-opus-5-5": 1.0 },
        })
    }

    #[test]
    fn a_run_is_a_row_per_model_with_the_workflow_off() {
        let (e, dir) = engine("rows", false);
        e.log_spend(&agent("b", 1.0), 3, true, &end());
        let rows = e.store.spend_since(0.0).unwrap();
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.model.as_str(), r.main, r.cost_usd, r.tokens_out))
            .collect();
        assert_eq!(
            got,
            [
                ("claude-haiku-5-5", false, Some(0.25), None),
                ("claude-opus-5-5", true, Some(1.0), Some(20)),
            ]
        );
        let r = &rows[1];
        assert_eq!(
            (r.tickets.as_str(), r.stage.as_str(), r.started_as.as_str()),
            ("SRE-1,SRE-2", "build", "opus")
        );
        assert_eq!(
            (r.run, r.duration_ms, r.source.as_str()),
            (3, Some(900), "run")
        );
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_group_agent_counts_on_its_groups_ticket() {
        let (e, dir) = engine("group", false);
        e.store
            .add_group("g", "", "SRE-9", "/tmp", "", false)
            .unwrap();
        let mut a = agent("b", 1.0);
        a.ticket.clear();
        e.log_spend(&a, 1, true, &end());
        assert_eq!(e.store.spend_since(0.0).unwrap()[0].tickets, "SRE-9");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cost_the_cli_did_not_report_is_unknown() {
        let (e, dir) = engine("unpriced", false);
        let mut a = agent("c", 1.0);
        a.harness = "codex".into();
        a.model = "gpt-5.5".into();
        let end = json!({ "state": "idle", "cost_usd": 0.0, "tokens": { "input": 5 } });
        e.log_spend(&a, 1, false, &end);
        let rows = e.store.spend_since(0.0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].cost_usd, rows[0].tokens_in), (None, Some(5)));
        assert_eq!(rows[0].model, "gpt-5.5");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reused_name_keeps_both_agents_runs() {
        let (e, dir) = engine("reused", false);
        e.log_spend(&agent("b", 1.0), 1, true, &end());
        // Finishing twice adds nothing.
        e.log_spend(&agent("b", 1.0), 1, true, &end());
        assert_eq!(e.store.spend_since(0.0).unwrap().len(), 2);
        e.log_spend(&agent("b", 2.0), 1, true, &end());
        assert_eq!(e.store.spend_since(0.0).unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_that_cannot_be_written_is_only_reported() {
        let (e, dir) = engine("unwritable", false);
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute_batch("DROP TABLE spend")
            .unwrap();
        e.log_spend(&agent("b", 1.0), 1, true, &end());
        assert!(e.store.spend_since(0.0).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn write(e: &Engine, folder: &str, ticket: &str, events: &[Value]) {
        let d = ledger::dir(&e.cfg, folder, ticket);
        std::fs::create_dir_all(&d).unwrap();
        let lines: Vec<String> = events.iter().map(Value::to_string).collect();
        std::fs::write(d.join(ledger::FILE), lines.join("\n") + "\n").unwrap();
    }

    fn run(agent: &str, n: i64, ts: f64, cost: f64) -> Vec<Value> {
        vec![
            json!({ "ts": ts - 5.0, "kind": "agent_started", "agent": agent, "stage": "build", "harness": "opencode" }),
            json!({ "ts": ts - 1.0, "kind": "run_start", "agent": agent, "run": n, "harness": "opencode", "model": "gw/anthropic/claude-opus-5.5", "prompt": "a secret prompt" }),
            json!({ "ts": ts, "kind": "run_end", "agent": agent, "role": "worker", "group": "g", "run": n, "cost_usd": cost, "state": "idle", "models": null }),
        ]
    }

    #[test]
    fn imports_recorded_runs_once() {
        let (e, dir) = engine("import", true);
        // The same run on two tickets, recorded a moment apart.
        write(&e, "g", "SRE-1", &run("b", 1, 100.0, 2.0));
        write(&e, "g", "SRE-2", &run("b", 1, 100.02, 2.0));
        // An archived ticket, and an unpriced run.
        write(&e, "archive", "SRE-3", &run("c", 1, 200.0, 0.0));
        assert_eq!(e.import_spend().unwrap(), 2);
        let rows = e.store.spend_since(0.0).unwrap();
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.agent.as_str(), r.tickets.as_str(), r.cost_usd))
            .collect();
        assert_eq!(got, [("b", "SRE-1,SRE-2", Some(2.0)), ("c", "SRE-3", None)]);
        let r = &rows[0];
        assert_eq!(
            (r.model.as_str(), r.harness.as_str(), r.stage.as_str()),
            ("claude-opus-5-5", "opencode", "build")
        );
        assert_eq!(
            (r.ts, r.source.as_str(), r.group_name.as_str()),
            (100.0, "import", "g")
        );
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"));
        write(&e, "g", "SRE-4", &run("d", 1, 300.0, 1.0));
        assert_eq!(e.import_spend().unwrap(), 0, "once per database");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_leaves_out_runs_already_logged() {
        let (e, dir) = engine("import-live", true);
        e.log_spend(&agent("b", 1.0), 1, true, &end());
        let now = crate::store::now();
        write(
            &e,
            "g",
            "SRE-1",
            &[run("b", 1, now, 1.25), run("b", 1, now - 9000.0, 4.0)].concat(),
        );
        assert_eq!(e.import_spend().unwrap(), 1, "only the earlier agent's run");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_imports_at_once_add_one_set() {
        let (e, dir) = engine("import-race", true);
        for n in 1..40 {
            write(
                &e,
                "g",
                &format!("SRE-{n}"),
                &run(&format!("a{n}"), 1, n as f64 * 100.0, 1.0),
            );
        }
        let added: usize = std::thread::scope(|s| {
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    let (cfg, db) = (e.cfg.clone(), dir.join("t.db"));
                    s.spawn(move || {
                        let store = Store::open(&db).unwrap();
                        Engine::with(store, cfg, "drovr".into())
                            .import_spend()
                            .unwrap()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(added, 39);
        assert_eq!(e.store.spend_since(0.0).unwrap().len(), 39);
        let _ = std::fs::remove_dir_all(dir);
    }
}
