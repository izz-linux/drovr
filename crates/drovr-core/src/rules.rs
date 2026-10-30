//! What the watcher does about a change it saw (`watch.rules`): `wake` the
//! agent that owns the work with what it needs to act (the failing log,
//! the review), `ask` you in the Inbox first, or `ignore` it. Each trigger
//! fires once per PR head, so a failure that stays failed doesn't wake an
//! agent again until it pushes.

use crate::engine::{truncate, Engine};
use crate::ledger;
use crate::shell::{self, quote};
use crate::store::Agent;
use crate::watch::{Pr, Ticket};
use serde_json::{json, Value};
use std::time::Duration;

/// The agent that should act on a ticket: its build stage, else a worker
/// or solo agent on it that isn't stopped (the most recent first), else its
/// group's lead.
pub fn owner_of(engine: &Engine, ticket: &str) -> Option<Agent> {
    let groups = engine.store.groups().unwrap_or_default();
    let mut on: Vec<Agent> = engine
        .store
        .agents(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            let own = ledger::tickets_in(&a.ticket);
            if !own.is_empty() {
                return own.iter().any(|k| k == ticket);
            }
            groups
                .iter()
                .find(|g| g.name == a.group_name)
                .is_some_and(|g| ledger::tickets_in(&g.ticket).iter().any(|k| k == ticket))
        })
        .collect();
    on.sort_by(|a, b| b.updated.total_cmp(&a.updated));
    let active = |a: &&Agent| !matches!(a.run_state.as_str(), "stopped");
    on.iter()
        .filter(active)
        .find(|a| a.stage == "build")
        .or_else(|| on.iter().filter(active).find(|a| a.role != "orchestrator"))
        .or_else(|| on.iter().find(|a| a.role == "orchestrator"))
        .cloned()
}

/// Act on what changed on a PR since the last poll.
pub fn on_pr(engine: &Engine, before: &Pr, pr: &mut Pr) {
    let green = |p: &Pr| {
        p.state == "OPEN"
            && !p.draft
            && p.review == "APPROVED"
            && matches!(p.ci.as_str(), "success" | "none")
            && p.mergeable == "MERGEABLE"
    };
    let mut triggers = vec![];
    if pr.ci == "failure" && before.ci != "failure" {
        triggers.push("ci_failed");
    }
    if pr.review == "CHANGES_REQUESTED" && before.review != "CHANGES_REQUESTED" {
        triggers.push("changes_requested");
    } else if pr.reviews > before.reviews && pr.review != "APPROVED" {
        // An approval is news for you (approved_green), not work for the agent.
        triggers.push("new_review");
    }
    if pr.mergeable == "CONFLICTING" && before.mergeable != "CONFLICTING" {
        triggers.push("conflict");
    }
    if green(pr) && !green(before) {
        triggers.push("approved_green");
    }
    if pr.state == "MERGED" && before.state != "MERGED" {
        triggers.push("merged");
    }
    if pr.state == "CLOSED" && before.state != "CLOSED" {
        triggers.push("pr_closed");
    }
    for trigger in triggers {
        let key = format!("{trigger}@{}", pr.head);
        if pr.fired.contains(&key) {
            continue;
        }
        pr.fired.push(key);
        let Some(ticket) = pr.tickets.first().cloned() else {
            continue;
        };
        let owner = owner_of(engine, &ticket);
        let (text, todo) = pr_text(engine, trigger, pr);
        let title = format!("{ticket} · PR #{}: {}", pr.number, headline(trigger));
        act(
            engine,
            trigger,
            &ticket,
            owner.as_ref(),
            &title,
            (&text, &todo),
            json!({ "pr": pr.url }),
        );
        if trigger == "merged" {
            draft_close(engine, &ticket, pr);
        }
    }
}

/// Act on what changed on a ticket in Jira since the last poll.
pub fn on_ticket(engine: &Engine, t: &Ticket, changes: &[(&'static str, Value)]) {
    let lines: Vec<String> = changes
        .iter()
        .filter_map(|(kind, d)| match *kind {
            "jira_status" => Some(format!(
                "- Status: {} → {}",
                d["from"].as_str()?,
                d["to"].as_str()?
            )),
            "jira_assignee" => Some(format!(
                "- Assignee: {} → {}",
                d["from"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("nobody"),
                d["to"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("nobody")
            )),
            "jira_comment" => Some(format!("- New comment: {}", d["latest"].as_str()?)),
            "sprint_left" => Some("- It left the sprint.".into()),
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        return;
    }
    let text = format!(
        "[drovr] {} changed in Jira ({}):\n{}\n{}",
        t.key,
        t.summary,
        lines.join("\n"),
        t.url
    );
    let owner = owner_of(engine, &t.key);
    let title = format!("{} changed in Jira", t.key);
    act(
        engine,
        "jira_changed",
        &t.key,
        owner.as_ref(),
        &title,
        (&text, ""),
        json!({}),
    );
}

fn headline(trigger: &str) -> &'static str {
    match trigger {
        "ci_failed" => "CI failed",
        "changes_requested" => "changes requested",
        "new_review" => "new review",
        "conflict" => "merge conflict",
        "approved_green" => "approved and green, ready to merge",
        "merged" => "merged",
        "pr_closed" => "closed without merging",
        _ => "changed",
    }
}

/// Wake the owner, or ask you, as `watch.rules` says for `trigger`. `what`
/// is what happened and what the owner should do about it: the agent gets
/// both; you see what happened, and what Send to would ask it.
fn act(
    engine: &Engine,
    trigger: &str,
    ticket: &str,
    owner: Option<&Agent>,
    title: &str,
    (happened, todo): (&str, &str),
    extra: Value,
) {
    let text = if todo.is_empty() {
        happened.to_string()
    } else {
        format!("{happened}\n{todo}")
    };
    let text = text.as_str();
    let mut action = engine.cfg.watch.rules.action(trigger).to_string();
    // A stopped agent isn't woken behind your back; nor is nobody.
    if action == "wake" && owner.is_none_or(|o| o.run_state == "stopped") {
        action = "ask".into();
    }
    match action.as_str() {
        "wake" => {
            let o = owner.expect("checked above");
            let mut data = json!({ "reason": trigger, "ticket": ticket, "by": "watcher" });
            merge(&mut data, extra);
            engine.record(&o.name, "wake", data);
            if let Err(e) = engine.prompt(&o.name, text) {
                eprintln!("drovr: couldn't wake {}: {e:#}", o.name);
            }
        }
        "ask" => {
            let options: Vec<String> = owner
                .map(|o| vec![format!("Send to {}", o.name)])
                .unwrap_or_default();
            let mut payload = json!({ "ticket": ticket, "trigger": trigger, "text": text, "agent": owner.map(|o| o.name.clone()) });
            merge(&mut payload, extra);
            let agent = owner.map(|o| o.name.as_str()).unwrap_or("watcher");
            let mut body = happened.trim_start_matches("[drovr] ").to_string();
            if let (Some(o), false) = (owner, todo.is_empty()) {
                body += &format!(
                    "\n\n**Send to {}** passes this on, asking it to: {todo}",
                    o.name
                );
            }
            match engine
                .store
                .add_item(agent, "watch", title, &body, &options, &payload)
            {
                Ok(id) => engine.record_item(id),
                Err(e) => eprintln!("drovr: couldn't add a watcher item: {e:#}"),
            }
        }
        _ => {}
    }
}

fn merge(into: &mut Value, from: Value) {
    if let (Some(o), Value::Object(f)) = (into.as_object_mut(), from) {
        o.extend(f);
    }
}

/// What happened on a PR, with the details the owner needs, and what it
/// should do about it.
fn pr_text(engine: &Engine, trigger: &str, pr: &Pr) -> (String, String) {
    let you = engine.cfg.your_name();
    let head = format!(
        "[drovr] PR #{} in {} ({}) for {}: {}.\n{}\n",
        pr.number,
        pr.repo,
        pr.title,
        pr.tickets.join(", "),
        headline(trigger),
        pr.url
    );
    let gh = quote(&engine.cfg.watch.gh);
    match trigger {
        "ci_failed" => {
            let mut s = format!("{head}\nFailing: {}\n", pr.failing.join(", "));
            if let Some(log) = failed_log(&gh, pr) {
                s += &format!("\nThe end of the failing log:\n```\n{log}\n```\n");
            } else if !pr.failing_urls.is_empty() {
                s += &format!("\nDetails: {}\n", pr.failing_urls.join(" "));
            }
            (s, "Find the cause, fix it on the PR's branch and push. If the failure isn't from this change, say so instead.".into())
        }
        "changes_requested" | "new_review" => {
            let mut s = head;
            if let Some(r) = reviews(&gh, pr) {
                s += &format!("\n{r}\n");
            }
            (
                s,
                "Address what the review asks for and push, or reply on the PR where you disagree."
                    .into(),
            )
        }
        "conflict" => (
            head,
            "Merge the base branch in, resolve the conflicts, run the checks, and push.".into(),
        ),
        "approved_green" => (
            head + "\nIt's approved, its checks pass and it has no conflicts.",
            String::new(),
        ),
        "merged" => (
            head,
            format!("Wrap up: say what's left on the ticket, and what {you} should know."),
        ),
        _ => (head, String::new()),
    }
}

/// The end of the first failing GitHub Actions run's log.
fn failed_log(gh: &str, pr: &Pr) -> Option<String> {
    let run = pr.failing_urls.iter().find_map(|u| {
        let rest = u.split("/actions/runs/").nth(1)?;
        let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (!id.is_empty()).then_some(id)
    })?;
    let cmd = format!("{gh} run view {run} -R {} --log-failed", quote(&pr.repo));
    let out = shell::sh(&cmd, None, None, Duration::from_secs(60)).ok()?;
    let lines: Vec<&str> = out.stdout.lines().collect();
    let tail = lines[lines.len().saturating_sub(150)..].join("\n");
    (!tail.trim().is_empty()).then(|| tail_chars(&tail, 12_000))
}

fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(n - max).collect::<String>())
    }
}

/// The latest reviews and review comments on a PR.
fn reviews(gh: &str, pr: &Pr) -> Option<String> {
    let mut out = String::new();
    let v = shell::sh(
        &format!("{gh} pr view {} --json reviews", quote(&pr.url)),
        None,
        None,
        Duration::from_secs(60),
    )
    .ok()
    .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok());
    for r in v
        .as_ref()
        .and_then(|v| v["reviews"].as_array())
        .into_iter()
        .flatten()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        out += &format!(
            "Review by {} ({}): {}\n",
            r["author"]["login"].as_str().unwrap_or("?"),
            r["state"].as_str().unwrap_or(""),
            truncate(r["body"].as_str().unwrap_or("").trim(), 2000)
        );
    }
    let c = shell::sh(
        &format!(
            "{gh} api {}",
            quote(&format!(
                "repos/{}/pulls/{}/comments?per_page=100",
                pr.repo, pr.number
            ))
        ),
        None,
        None,
        Duration::from_secs(60),
    )
    .ok()
    .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok());
    let comments: Vec<&Value> = c
        .as_ref()
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .collect();
    for cm in comments.iter().rev().take(10).rev() {
        out += &format!(
            "- {}:{} {}: {}\n",
            cm["path"].as_str().unwrap_or(""),
            cm["line"]
                .as_u64()
                .or_else(|| cm["original_line"].as_u64())
                .unwrap_or(0),
            cm["user"]["login"].as_str().unwrap_or("?"),
            truncate(cm["body"].as_str().unwrap_or("").trim(), 1000)
        );
    }
    (!out.is_empty()).then_some(out)
}

/// When a ticket's PR merges, draft the Jira update for you to post: a
/// comment, and the move to `watch.merged_transition` if it's set.
fn draft_close(engine: &Engine, ticket: &str, pr: &Pr) {
    if !engine.cfg.jira.comment_cmd.trim().is_empty() {
        let body = format!("Merged {}#{}: {}\n{}", pr.repo, pr.number, pr.title, pr.url);
        let _ = draft_jira(engine, "watcher", ticket, "comment", &body, "", "");
    }
    let to = engine.cfg.watch.merged_transition.trim();
    if !to.is_empty() && !engine.cfg.jira.transition_cmd.trim().is_empty() {
        let _ = draft_jira(engine, "watcher", ticket, "transition", "", to, "");
    }
}

/// When you dispatch a ticket, move it with `watch.dispatch_transition`
/// (say, to In Progress), unless the watcher last saw it already started.
/// It's posted straight away; if Jira refuses, the move waits in the Inbox
/// as a draft, with the error, for you to fix and post or dismiss.
pub fn move_on_dispatch(engine: &Engine, key: &str) {
    move_on_dispatch_seen(engine, key, &crate::watch::load().tickets);
}

fn move_on_dispatch_seen(
    engine: &Engine,
    key: &str,
    seen: &std::collections::BTreeMap<String, Ticket>,
) {
    let to = engine.cfg.watch.dispatch_transition.trim();
    if !engine.cfg.workflow.enabled
        || to.is_empty()
        || engine.cfg.jira.transition_cmd.trim().is_empty()
    {
        return;
    }
    let key = key.trim().to_uppercase();
    // Started or done already: moving it back to In Progress would be wrong.
    if seen
        .get(&key)
        .is_some_and(|t| !matches!(t.category.as_str(), "" | "new"))
    {
        return;
    }
    match crate::sources::Sources::new(&engine.cfg).jira_transition(&key, to, "") {
        Ok(_) => engine.record_ticket(
            &key,
            "jira_moved",
            json!({ "transition": to, "on": "dispatch", "by": "drovr" }),
        ),
        Err(e) => {
            let body = format!("Moving {key} when you dispatched it failed:\n{e:#}");
            let _ = draft_jira(engine, "watcher", &key, "transition", &body, to, "");
        }
    }
}

/// A Jira update for you to edit and post: a comment, or a move to
/// `transition` (with `fields`, JSON, when it needs any).
pub fn draft_jira(
    engine: &Engine,
    agent: &str,
    ticket: &str,
    action: &str,
    body: &str,
    transition: &str,
    fields: &str,
) -> anyhow::Result<i64> {
    let title = match action {
        "comment" => format!("{ticket}: comment"),
        _ => format!("{ticket}: move to {transition}"),
    };
    let id = engine.store.add_item(
        agent,
        "jira",
        &title,
        body,
        &[],
        &json!({ "ticket": ticket, "action": action, "body": body, "transition": transition, "fields": fields }),
    )?;
    engine.record_item(id);
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;

    fn setup(name: &str) -> (std::path::PathBuf, Engine) {
        let dir = std::env::temp_dir().join(format!("drovr-rules-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.workflow.enabled = true;
        cfg.defaults.auto_wake = false;
        cfg.reports.dir = dir.join("plans").display().to_string();
        cfg.watch.gh = "false".into();
        cfg.jira.comment_cmd = "echo {body}".into();
        cfg.jira.transition_cmd = "echo {transition}".into();
        cfg.watch.merged_transition = "Complete".into();
        let engine = Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into());
        engine
            .store
            .add_group("wif", "", "SRE-3229", "/tmp", "", false)
            .unwrap();
        (dir, engine)
    }

    fn pr(ci: &str, review: &str, mergeable: &str, state: &str) -> Pr {
        Pr {
            url: "https://github.com/o/r/pull/7".into(),
            repo: "o/r".into(),
            number: 7,
            title: "feat(SRE-3229): kinds".into(),
            tickets: vec!["SRE-3229".into()],
            state: state.into(),
            ci: ci.into(),
            review: review.into(),
            mergeable: mergeable.into(),
            head: "a1".into(),
            ..Pr::default()
        }
    }

    #[test]
    fn picks_who_acts() {
        let (dir, engine) = setup("owner");
        let add = |name: &str, role: &str, state: &str| {
            engine
                .store
                .add_agent(&Agent {
                    name: name.into(),
                    group_name: "wif".into(),
                    role: role.into(),
                    run_state: state.into(),
                    ..Agent::default()
                })
                .unwrap()
        };
        assert!(owner_of(&engine, "SRE-3229").is_none());
        add("wif-lead", "orchestrator", "idle");
        assert_eq!(owner_of(&engine, "SRE-3229").unwrap().name, "wif-lead");
        add("wif-api", "worker", "idle");
        assert_eq!(owner_of(&engine, "SRE-3229").unwrap().name, "wif-api");
        engine
            .store
            .set_agent("wif-api", "run_state", "stopped")
            .unwrap();
        assert_eq!(owner_of(&engine, "SRE-3229").unwrap().name, "wif-lead");
        assert!(owner_of(&engine, "SRE-1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asks_once_per_head_and_drafts_jira_on_merge() {
        let (dir, mut engine) = setup("act");
        // Nobody's on it, so even a wake rule asks you.
        let before = pr("pending", "REVIEW_REQUIRED", "MERGEABLE", "OPEN");
        let mut now = pr("failure", "REVIEW_REQUIRED", "MERGEABLE", "OPEN");
        on_pr(&engine, &before, &mut now);
        on_pr(&engine, &before, &mut now);
        let items = engine.store.items(Some("pending"), 50).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            (items[0].kind.as_str(), items[0].agent.as_str()),
            ("watch", "watcher")
        );
        assert!(items[0].title.contains("CI failed"));
        assert_eq!(now.fired, ["ci_failed@a1"]);

        // Approved and green, then merged: an ask each, plus the Jira drafts.
        // The approving review itself doesn't count as a new review.
        let mut green = pr("success", "APPROVED", "MERGEABLE", "OPEN");
        green.reviews = 1;
        let mut g = green.clone();
        on_pr(&engine, &now, &mut g);
        let mut merged = pr("success", "APPROVED", "MERGEABLE", "MERGED");
        merged.fired = g.fired.clone();
        on_pr(&engine, &green, &mut merged);
        let kinds: Vec<String> = engine
            .store
            .items(Some("pending"), 50)
            .unwrap()
            .iter()
            .map(|i| format!("{}:{}", i.kind, i.title))
            .collect();
        assert!(kinds.iter().any(|k| k.contains("ready to merge")));
        assert!(kinds
            .iter()
            .any(|k| k.starts_with("watch:") && k.contains("merged")));
        assert!(kinds.contains(&"jira:SRE-3229: comment".to_string()));
        assert!(kinds.contains(&"jira:SRE-3229: move to Complete".to_string()));
        // Posting a draft runs jira.comment_cmd with your edited text.
        let pending = |engine: &Engine| engine.store.items(Some("pending"), 50).unwrap();
        let comment = pending(&engine)
            .into_iter()
            .find(|i| i.title == "SRE-3229: comment")
            .unwrap();
        engine.answer(comment.id, "Merged, thanks all").unwrap();
        let posted = engine.store.item(comment.id).unwrap().unwrap();
        assert_eq!(
            (posted.status.as_str(), posted.answer.as_str()),
            ("answered", "Posted:\nMerged, thanks all")
        );
        // A failing post leaves the draft in the Inbox.
        engine.cfg.jira.transition_cmd = "false".into();
        let mv = pending(&engine)
            .into_iter()
            .find(|i| i.kind == "jira")
            .unwrap();
        assert!(engine.answer(mv.id, "Complete").is_err());
        assert_eq!(engine.store.item(mv.id).unwrap().unwrap().status, "pending");
        // All of it is in the ticket's record.
        let t = ledger::timeline(&engine.cfg, "SRE-3229");
        assert_eq!(t.iter().filter(|e| e["kind"] == "asked").count(), 5);
        assert_eq!(t.iter().filter(|e| e["kind"] == "answered").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moves_a_dispatched_ticket_unless_it_already_started() {
        let (dir, mut engine) = setup("dispatch");
        let kinds = |engine: &Engine| -> Vec<String> {
            ledger::timeline(&engine.cfg, "SRE-3229")
                .iter()
                .map(|e| e["kind"].as_str().unwrap_or("").to_string())
                .collect()
        };
        let seen = |category: &str| {
            std::collections::BTreeMap::from([(
                "SRE-3229".to_string(),
                Ticket {
                    key: "SRE-3229".into(),
                    category: category.into(),
                    ..Ticket::default()
                },
            )])
        };
        // Not set: nothing happens.
        move_on_dispatch_seen(&engine, "sre-3229", &seen("new"));
        assert!(kinds(&engine).is_empty());
        engine.cfg.watch.dispatch_transition = "21".into();
        // Already in progress (or done): left alone.
        move_on_dispatch_seen(&engine, "SRE-3229", &seen("indeterminate"));
        assert!(kinds(&engine).is_empty());
        // To do, or not seen by the watcher: moved, and recorded.
        move_on_dispatch_seen(&engine, "SRE-3229", &seen("new"));
        move_on_dispatch_seen(&engine, "SRE-3229", &Default::default());
        assert_eq!(kinds(&engine), ["jira_moved", "jira_moved"]);
        // Jira refuses: a draft with the error, to post or dismiss.
        engine.cfg.jira.transition_cmd = "echo no such transition >&2; false".into();
        move_on_dispatch_seen(&engine, "SRE-3229", &seen("new"));
        let draft = engine
            .store
            .items(Some("pending"), 50)
            .unwrap()
            .into_iter()
            .find(|i| i.kind == "jira")
            .unwrap();
        assert_eq!(draft.payload["transition"], "21");
        assert!(draft.body.contains("no such transition"), "{}", draft.body);
        // Off with the workflow.
        engine.cfg.workflow.enabled = false;
        engine.cfg.jira.transition_cmd = "echo {transition}".into();
        move_on_dispatch_seen(&engine, "SRE-3229", &seen("new"));
        assert_eq!(
            kinds(&engine).iter().filter(|k| *k == "jira_moved").count(),
            2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asks_you_with_what_happened_and_what_send_to_would_ask() {
        let (dir, mut engine) = setup("ask-wording");
        engine.cfg.defaults.your_name = "Izz".into();
        engine
            .store
            .add_agent(&Agent {
                name: "wif-api".into(),
                group_name: "wif".into(),
                role: "worker".into(),
                run_state: "working".into(),
                ..Agent::default()
            })
            .unwrap();
        let before = pr("success", "APPROVED", "MERGEABLE", "OPEN");
        let mut now = pr("success", "APPROVED", "MERGEABLE", "MERGED");
        on_pr(&engine, &before, &mut now);
        let item = engine
            .store
            .items(Some("pending"), 50)
            .unwrap()
            .into_iter()
            .find(|i| i.kind == "watch")
            .unwrap();
        // You read what happened, and what Send to would ask of the agent.
        assert!(item.body.starts_with("PR #7 in o/r"), "{}", item.body);
        assert!(item.body.ends_with(
            "**Send to wif-api** passes this on, asking it to: Wrap up: say what's left on the ticket, and what Izz should know."
        ), "{}", item.body);
        assert!(!item.body.contains("the human"));
        // The agent gets the whole of it.
        let text = item.payload["text"].as_str().unwrap();
        assert!(
            text.starts_with("[drovr] PR #7") && text.ends_with("what Izz should know."),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wakes_the_owner_with_what_changed() {
        let (dir, mut engine) = setup("wake");
        // Waking prompts; with a busy agent the prompt is queued, so nothing runs.
        engine
            .store
            .add_agent(&Agent {
                name: "wif-api".into(),
                group_name: "wif".into(),
                role: "worker".into(),
                run_state: "working".into(),
                ..Agent::default()
            })
            .unwrap();
        engine.cfg.watch.rules.conflict = "wake".into();
        let before = pr("success", "REVIEW_REQUIRED", "MERGEABLE", "OPEN");
        let mut now = pr("success", "REVIEW_REQUIRED", "CONFLICTING", "OPEN");
        on_pr(&engine, &before, &mut now);
        let queued = engine.store.queued("wif-api").unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].body.contains("merge conflict"));
        assert!(queued[0].body.contains("Merge the base branch in"));
        let wake = ledger::timeline(&engine.cfg, "SRE-3229")
            .into_iter()
            .find(|e| e["kind"] == "wake")
            .unwrap();
        assert_eq!(
            (wake["reason"].as_str(), wake["by"].as_str()),
            (Some("conflict"), Some("watcher"))
        );
        // Ignored triggers do nothing.
        engine.cfg.watch.rules.approved_green = "ignore".into();
        let mut g = pr("success", "APPROVED", "MERGEABLE", "OPEN");
        on_pr(&engine, &now, &mut g);
        assert!(engine.store.items(Some("pending"), 50).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
