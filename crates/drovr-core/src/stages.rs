//! Stages, part of the ticket workflow: a group's lead spawns one agent per
//! stage (`stages.order`), each on its stage's model. A stage ends with a
//! handoff, a written summary the next stage starts from, and you approve
//! every one in the Inbox: approving stops the stage's agent and tells the
//! lead to start the next; sending it back returns it with your notes.
//!
//! The build stage, the heaviest, checkpoints when its session passes
//! `stages.build_checkpoint_tokens` of context or `build_checkpoint_usd`:
//! it writes down where it is, and its next run starts a fresh session
//! from the checkpoint, the handoffs and the decisions so far.

use crate::engine::{capitalize, truncate, Engine};
use crate::ledger;
use crate::store::{Agent, Item};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const CHECKPOINTING: &str = "checkpointing";
const REHYDRATE: &str = "rehydrate";
const FRESH: &str = "fresh";

impl Engine {
    /// Where an agent's ticket records are: its group's plans folder (or
    /// the plans folder itself) and its tickets, its own or else its group's.
    pub fn ticket_place(&self, agent: &Agent) -> (String, Vec<String>) {
        let g = match agent.group_name.as_str() {
            "" => None,
            g => self.store.group(g).ok().flatten(),
        };
        let mut keys = ledger::tickets_in(&agent.ticket);
        if keys.is_empty() {
            keys = g
                .as_ref()
                .map(|g| ledger::tickets_in(&g.ticket))
                .unwrap_or_default();
        }
        let folder = g.map(|g| g.plans_folder().to_string()).unwrap_or_default();
        (folder, keys)
    }

    /// A stage agent hands its stage off: drovr writes the handoff into the
    /// ticket's folder and asks you to approve it.
    pub fn handoff(&self, me: &Agent, a: &Value) -> Result<Value> {
        if me.stage.is_empty() {
            bail!("only a stage's agent hands off; you have no stage");
        }
        let text = |k: &str| a[k].as_str().unwrap_or("").trim().to_string();
        if text("summary").is_empty() || text("next_inputs").is_empty() {
            bail!("summary and next_inputs are required");
        }
        let (folder, keys) = self.ticket_place(me);
        let ticket = keys
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("you have no ticket"))?;
        let next = self.cfg.stages.next(&me.stage).map(String::from);
        let doc = format!(
            "# {ticket} · {stage} handoff\n\nBy `{agent}`, {at}.\n\n## Summary\n{summary}\n\n## Outputs\n{outputs}\n\n## Decisions\n{decisions}\n\n## Open questions\n{questions}\n\n## Inputs for the next stage{next}\n{inputs}\n",
            stage = me.stage,
            agent = me.name,
            at = ledger::iso(crate::store::now()),
            summary = text("summary"),
            outputs = or_none(&text("outputs")),
            decisions = or_none(&text("decisions")),
            questions = or_none(&text("open_questions")),
            next = next.as_ref().map(|n| format!(" ({n})")).unwrap_or_default(),
            inputs = text("next_inputs"),
        );
        let dir = ledger::dir(&self.cfg, &folder, &ticket).join("handoffs");
        std::fs::create_dir_all(&dir)?;
        let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) + 1;
        let file = dir.join(format!("{n:02}-{}.md", me.stage));
        std::fs::write(&file, ledger::redact(&doc))?;
        let lead = self.lead_of(&me.group_name).map(|l| l.name);
        self.record(
            &me.name,
            "handoff",
            json!({ "stage": me.stage, "next": next, "file": file, "summary": truncate(&text("summary"), 2000) }),
        );
        let title = match &next {
            Some(n) => format!("{ticket} · {} → {n}: approve the handoff?", me.stage),
            None => format!(
                "{ticket} · {}: approve, and the ticket's stages are done?",
                me.stage
            ),
        };
        let id = self.store.add_item(
            &me.name,
            "handoff",
            &title,
            &truncate(&doc, 12_000),
            &["Approve".into(), "Send back".into()],
            &json!({
                "ticket": ticket,
                "stage": me.stage,
                "next": next,
                "file": file,
                "lead": lead,
                "next_model": next.as_deref().map(|n| self.cfg.stages.model(n)).unwrap_or(""),
            }),
        )?;
        self.record_item(id);
        Ok(
            json!({ "handoff": file, "inbox": id, "note": format!("Waiting on {}'s approval. End your turn now; your stage is over once they approve.", self.cfg.your_name()) }),
        )
    }

    /// The group's lead, if it has one.
    pub fn lead_of(&self, group: &str) -> Option<Agent> {
        if group.is_empty() {
            return None;
        }
        self.store
            .agents(Some(group))
            .ok()?
            .into_iter()
            .find(|a| a.role == "orchestrator")
    }

    /// You approved a handoff (`answer` is "Approve", optionally with
    /// "model=<name>" for the next stage): the stage's agent stops, and the
    /// lead is told to start the next stage.
    pub(crate) fn approve_handoff(&self, item: &Item, answer: &str) -> Result<()> {
        let p = &item.payload;
        let stage = p["stage"].as_str().unwrap_or("");
        let file = p["file"].as_str().unwrap_or("");
        let model = answer
            .split_once("model=")
            .map(|(_, m)| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .or_else(|| {
                p["next_model"]
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .map(String::from)
            });
        let me = self.agent(&item.agent)?;
        let _ = self.stop(&me.name);
        self.record(
            &me.name,
            "handoff_approved",
            json!({ "stage": stage, "next": p["next"], "model": model, "by": "you" }),
        );
        let you = capitalize(&self.cfg.your_name());
        match p["next"].as_str() {
            Some(next) => {
                let text = format!(
                    "[drovr] {you} approved the {stage} handoff: {file}\n\nStart the {next} stage now: call spawn_worker with stage \"{next}\"{model}, and a task that starts from that handoff file (it should read it first). Add anything else it needs from the earlier stages.",
                    model = model.as_ref().map(|m| format!(", model \"{m}\"")).unwrap_or_default(),
                );
                match self.lead_of(&me.group_name) {
                    Some(lead) => {
                        self.prompt(&lead.name, &text)?;
                    }
                    // No lead: drovr starts the next stage itself.
                    None => {
                        self.start_agent(crate::engine::NewAgent {
                            name: format!("{}-{next}", me.name.trim_end_matches(&format!("-{stage}"))),
                            group: me.group_name.clone(),
                            role: me.role.clone(),
                            backend: me.backend.clone(),
                            harness: me.harness.clone(),
                            cwd: me.cwd.clone(),
                            ticket: me.ticket.clone(),
                            task: format!("You're the {next} stage. Start from the {stage} handoff: read {file} first."),
                            stage: next.into(),
                            model: model.unwrap_or_default(),
                            ..Default::default()
                        })?;
                    }
                }
            }
            None => {
                if self.cfg.stages.tidy_on_close {
                    for a in self.store.agents(Some(&me.group_name))? {
                        if !a.stage.is_empty() && a.run_state == "stopped" {
                            let _ = self.remove(&a.name);
                        }
                    }
                }
                if let Some(lead) = self.lead_of(&me.group_name) {
                    self.prompt(
                        &lead.name,
                        &format!("[drovr] {you} approved the last stage ({stage}): {file}\n\nThe ticket's stages are done. Wrap up with report_done."),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// You sent a handoff back: the stage's agent gets your notes.
    pub(crate) fn send_back_handoff(&self, item: &Item, notes: &str) -> Result<()> {
        let stage = item.payload["stage"].as_str().unwrap_or("");
        self.record(
            &item.agent,
            "handoff_sent_back",
            json!({ "stage": stage, "notes": truncate(notes, 4000), "by": "you" }),
        );
        let notes = if notes.trim().is_empty() {
            "(no notes)"
        } else {
            notes
        };
        self.prompt(
            &item.agent,
            &format!(
                "[drovr] {} sent your {stage} handoff back:\n\n{notes}\n\nAddress it, then call handoff again.",
                capitalize(&self.cfg.your_name())
            ),
        )?;
        Ok(())
    }

    /// The build stage writes down where it is; its next run starts fresh
    /// from it.
    pub fn checkpoint(&self, me: &Agent, a: &Value) -> Result<Value> {
        let text = |k: &str| a[k].as_str().unwrap_or("").trim().to_string();
        if text("done").is_empty() || text("left").is_empty() {
            bail!("done and left are required");
        }
        let (folder, keys) = self.ticket_place(me);
        let ticket = keys
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("you have no ticket"))?;
        let dir = ledger::dir(&self.cfg, &folder, &ticket).join("checkpoints");
        std::fs::create_dir_all(&dir)?;
        let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) + 1;
        let stage = if me.stage.is_empty() {
            "work"
        } else {
            &me.stage
        };
        let file = dir.join(format!("{stage}-{n:02}.md"));
        let doc = format!(
            "# {ticket} · {stage} checkpoint {n}\n\nBy `{}`, {}.\n\n## Done\n{}\n\n## Left\n{}\n\n## State (branch, PR, tests)\n{}\n\n## Decisions since the blueprint\n{}\n",
            me.name,
            ledger::iso(crate::store::now()),
            text("done"),
            text("left"),
            or_none(&text("state")),
            or_none(&text("decisions")),
        );
        std::fs::write(&file, ledger::redact(&doc))?;
        let agent_dir = self.agent_dir(&me.name);
        let _ = std::fs::remove_file(agent_dir.join(CHECKPOINTING));
        std::fs::write(agent_dir.join(REHYDRATE), file.display().to_string())?;
        self.record(&me.name, "checkpoint", json!({ "file": file, "n": n }));
        Ok(
            json!({ "checkpoint": file, "note": "Saved. End your turn: drovr continues you in a fresh session that starts from it." }),
        )
    }

    /// After a stage agent's run: ask the build stage for a checkpoint when
    /// its session is past a limit, or continue it fresh from one it wrote.
    /// True when it was prompted, so nothing else should wake it now.
    pub(crate) fn after_stage_run(&self, agent: &Agent, end: &Value, log: &str) -> Result<bool> {
        if !self.cfg.workflow.enabled || agent.stage.is_empty() {
            return Ok(false);
        }
        let dir = self.agent_dir(&agent.name);
        if let Ok(path) = std::fs::read_to_string(dir.join(REHYDRATE)) {
            let _ = std::fs::remove_file(dir.join(REHYDRATE));
            std::fs::write(dir.join(FRESH), "")?;
            let text = self.rehydration(agent, Path::new(path.trim()));
            self.record(&agent.name, "rehydrated", json!({ "from": path.trim() }));
            self.prompt(&agent.name, &text)?;
            return Ok(true);
        }
        if agent.stage != "build" || dir.join(CHECKPOINTING).exists() {
            return Ok(false);
        }
        let s = &self.cfg.stages;
        let tokens = crate::harness::of(agent).context_tokens(log).unwrap_or(0);
        let cost = end["total_cost_usd"].as_f64().unwrap_or(0.0);
        let over_tokens = s.build_checkpoint_tokens > 0 && tokens >= s.build_checkpoint_tokens;
        let over_cost = s.build_checkpoint_usd > 0.0 && cost >= s.build_checkpoint_usd;
        if !(over_tokens || over_cost) {
            return Ok(false);
        }
        std::fs::write(dir.join(CHECKPOINTING), "")?;
        self.record(
            &agent.name,
            "checkpoint_due",
            json!({ "context_tokens": tokens, "session_usd": cost, "limit_tokens": s.build_checkpoint_tokens, "limit_usd": s.build_checkpoint_usd }),
        );
        self.prompt(
            &agent.name,
            &format!(
                "[drovr] Your session is getting large ({tokens} tokens of context, ${cost:.2} so far). Write a checkpoint now with the `checkpoint` tool: what's done, what's left, the state of the branch, PR and tests, and decisions made since the blueprint. Then end your turn: drovr continues you in a fresh session that starts from it."
            ),
        )?;
        Ok(true)
    }

    /// A stage agent whose next run starts a fresh session (after a
    /// checkpoint): a new session ID, and no resumed-session marker.
    pub(crate) fn take_fresh(&self, agent: &Agent) -> Result<Option<Agent>> {
        let dir = self.agent_dir(&agent.name);
        if std::fs::remove_file(dir.join(FRESH)).is_err() {
            return Ok(None);
        }
        let _ = std::fs::remove_file(dir.join("resumed"));
        let id = if crate::harness::of(agent).picks_session_id() {
            uuid::Uuid::new_v4().to_string()
        } else {
            String::new()
        };
        self.store.set_agent(&agent.name, "session_id", &id)?;
        Ok(Some(self.agent(&agent.name)?))
    }

    /// What a fresh build session starts from.
    fn rehydration(&self, agent: &Agent, checkpoint: &Path) -> String {
        let (folder, keys) = self.ticket_place(agent);
        let ticket = keys.first().cloned().unwrap_or_default();
        let dir = ledger::dir(&self.cfg, &folder, &ticket);
        let mut handoffs: Vec<PathBuf> = std::fs::read_dir(dir.join("handoffs"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        handoffs.sort();
        let decisions: Vec<String> = ledger::read(&dir)
            .iter()
            .filter_map(|e| match e["kind"].as_str()? {
                "decision" => Some(format!(
                    "- {} → {}",
                    e["question"].as_str()?,
                    e["chosen"].as_str()?
                )),
                "answered" if e["type"] == "decision" => Some(format!(
                    "- {} → {} (answered by {})",
                    e["title"].as_str()?,
                    e["answer"].as_str()?,
                    self.cfg.your_name()
                )),
                _ => None,
            })
            .collect();
        let watch = crate::watch::load();
        let prs: Vec<String> = watch
            .prs
            .values()
            .filter(|p| p.tickets.contains(&ticket))
            .map(|p| {
                format!(
                    "- #{} {} ({}, checks {}, review {}): {}",
                    p.number,
                    p.title,
                    p.state,
                    p.ci,
                    if p.review.is_empty() {
                        "none"
                    } else {
                        &p.review
                    },
                    p.url
                )
            })
            .collect();
        let summary = watch
            .tickets
            .get(&ticket)
            .map(|t| format!(" ({}, {})", t.summary, t.status))
            .unwrap_or_default();
        let mut s = format!(
            "[drovr] You're continuing {ticket}{summary} as its {stage} stage, in a fresh session. Your last session was checkpointed. Read the files below before you carry on.\n\nYour checkpoint ({cp}):\n\n{body}\n",
            stage = agent.stage,
            cp = checkpoint.display(),
            body = std::fs::read_to_string(checkpoint).unwrap_or_default(),
        );
        if !handoffs.is_empty() {
            s += "\nThe handoffs so far (the blueprint is your spec):\n";
            for h in &handoffs {
                s += &format!("- {}\n", h.display());
            }
        }
        if !decisions.is_empty() {
            s += &format!("\nDecisions so far:\n{}\n", decisions.join("\n"));
        }
        if !prs.is_empty() {
            s += &format!("\nPull requests:\n{}\n", prs.join("\n"));
        }
        s
    }
}

fn or_none(s: &str) -> String {
    if s.is_empty() {
        "None.".into()
    } else {
        s.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;

    /// An engine with a group on SRE-3229, its lead, and nothing that may
    /// start a run: every agent is "working", so prompts queue.
    fn setup(name: &str) -> (PathBuf, Engine, String) {
        let id = format!("{name}{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("drovr-stages-{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.workflow.enabled = true;
        cfg.defaults.auto_wake = false;
        cfg.orchestrator.auto_spawn = false;
        cfg.reports.dir = dir.join("plans").display().to_string();
        cfg.stages
            .models
            .insert("research".into(), "claude-haiku-5-5".into());
        let engine = Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into());
        engine
            .store
            .add_group("g", "", "SRE-3229", "/tmp", "", true)
            .unwrap();
        let lead = format!("lead-{id}");
        engine
            .store
            .add_agent(&Agent {
                name: lead.clone(),
                group_name: "g".into(),
                role: "orchestrator".into(),
                run_state: "working".into(),
                ..Agent::default()
            })
            .unwrap();
        (dir, engine, id)
    }

    fn stage_agent(engine: &Engine, name: &str, stage: &str) -> Agent {
        engine
            .store
            .add_agent(&Agent {
                name: name.into(),
                group_name: "g".into(),
                role: "worker".into(),
                stage: stage.into(),
                run_state: "working".into(),
                ..Agent::default()
            })
            .unwrap();
        engine.agent(name).unwrap()
    }

    fn cleanup(dir: &Path, engine: &Engine) {
        for a in engine.store.agents(None).unwrap() {
            let _ = std::fs::remove_dir_all(engine.agent_dir(&a.name));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_handoff_waits_for_you_then_the_lead_starts_the_next_stage() {
        let (dir, engine, id) = setup("approve");
        let triage = stage_agent(&engine, &format!("triage-{id}"), "triage");
        let r = engine.handoff(&triage, &json!({ "summary": "Scope is two repos", "next_inputs": "Look at acme-github kinds" })).unwrap();
        let file = PathBuf::from(r["handoff"].as_str().unwrap());
        assert!(file.ends_with("g/SRE-3229/handoffs/01-triage.md"));
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("## Inputs for the next stage (research)"));
        let item = engine
            .store
            .item(r["inbox"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            (item.kind.as_str(), item.payload["next_model"].as_str()),
            ("handoff", Some("claude-haiku-5-5"))
        );

        engine.answer(item.id, "Approve").unwrap();
        assert_eq!(engine.agent(&triage.name).unwrap().run_state, "stopped");
        let told = engine.store.queued(&format!("lead-{id}")).unwrap();
        assert!(told[0].body.contains("Start the research stage now"));
        assert!(told[0].body.contains("model \"claude-haiku-5-5\""));
        let kinds: Vec<String> = ledger::timeline(&engine.cfg, "SRE-3229")
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, ["handoff", "asked", "answered", "handoff_approved"]);

        // The lead spawns the next stage on the stage's model.
        let spawned = engine
            .spawn_worker(
                &format!("lead-{id}"),
                "",
                "Start from the triage handoff",
                "",
                "research",
                "",
            )
            .unwrap();
        let req = engine
            .store
            .item(spawned["pending_approval"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            (req.payload["stage"].as_str(), req.payload["name"].as_str()),
            (Some("research"), Some("g-research"))
        );
        assert!(engine
            .spawn_worker(&format!("lead-{id}"), "", "x", "", "deploy", "")
            .is_err());
        cleanup(&dir, &engine);
    }

    #[test]
    fn sending_back_returns_your_notes_and_closing_tidies_up() {
        let (dir, engine, id) = setup("back");
        let close = stage_agent(&engine, &format!("close-{id}"), "close");
        let first = engine
            .handoff(&close, &json!({ "summary": "Done", "next_inputs": "-" }))
            .unwrap();
        engine
            .reject(first["inbox"].as_i64().unwrap(), "Add the rollout notes")
            .unwrap();
        let back = engine.store.queued(&close.name).unwrap();
        assert!(back[0].body.contains("Add the rollout notes"));
        // The last stage: approving stops it, removes it, and tells the lead.
        let second = engine
            .handoff(
                &close,
                &json!({ "summary": "Done, with notes", "next_inputs": "-" }),
            )
            .unwrap();
        assert!(PathBuf::from(second["handoff"].as_str().unwrap()).ends_with("02-close.md"));
        engine
            .answer(second["inbox"].as_i64().unwrap(), "Approve")
            .unwrap();
        assert!(engine.store.agent(&close.name).unwrap().is_none());
        assert!(engine.store.queued(&format!("lead-{id}")).unwrap()[0]
            .body
            .contains("stages are done"));
        let kinds: Vec<String> = ledger::timeline(&engine.cfg, "SRE-3229")
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect();
        assert!(kinds.contains(&"handoff_sent_back".to_string()));
        assert!(kinds.contains(&"agent_removed".to_string()));
        cleanup(&dir, &engine);
    }

    #[test]
    fn the_build_stage_checkpoints_and_continues_fresh() {
        let (dir, mut engine, id) = setup("build");
        engine.cfg.stages.build_checkpoint_tokens = 1000;
        engine.cfg.stages.build_checkpoint_usd = 0.0;
        let build = stage_agent(&engine, &format!("build-{id}"), "build");
        engine
            .store
            .set_agent(&build.name, "session_id", "old-session")
            .unwrap();
        let build = engine.agent(&build.name).unwrap();
        let small = r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":500}}}"#;
        let big = r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":900,"cache_creation_input_tokens":200}}}"#;
        assert!(!engine.after_stage_run(&build, &json!({}), small).unwrap());
        assert!(engine.after_stage_run(&build, &json!({}), big).unwrap());
        assert!(engine.store.queued(&build.name).unwrap()[0]
            .body
            .contains("Write a checkpoint now"));
        // Asked once until it writes one.
        assert!(!engine.after_stage_run(&build, &json!({}), big).unwrap());

        engine.record(
            &build.name,
            "decision",
            json!({ "question": "Lib?", "chosen": "serde" }),
        );
        let cp = engine
            .checkpoint(
                &build,
                &json!({ "done": "API and tests", "left": "UI", "state": "PR #7 green" }),
            )
            .unwrap();
        assert!(
            PathBuf::from(cp["checkpoint"].as_str().unwrap()).ends_with("checkpoints/build-01.md")
        );
        // Its next end of run continues it fresh, from the checkpoint.
        assert!(engine.after_stage_run(&build, &json!({}), small).unwrap());
        let rehydrate = &engine.store.queued(&build.name).unwrap()[1].body;
        assert!(rehydrate.contains("in a fresh session"));
        assert!(rehydrate.contains("API and tests"));
        assert!(rehydrate.contains("- Lib? → serde"));
        let fresh = engine.take_fresh(&build).unwrap().unwrap();
        assert_ne!(fresh.session_id, "old-session");
        assert!(engine.take_fresh(&fresh).unwrap().is_none());
        let kinds: Vec<String> = ledger::timeline(&engine.cfg, "SRE-3229")
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            ["checkpoint_due", "decision", "checkpoint", "rehydrated"]
        );
        cleanup(&dir, &engine);
    }
}
