//! The watcher, part of the ticket workflow: the status of your sprint's
//! tickets and their pull requests, kept without an agent.
//!
//! Each poll reads the tickets matching `watch.jql` through the Jira
//! `command` provider (twg) or `rest`, with a full read (comments) only for
//! tickets whose `updated` time moved. It then searches `watch.github_repos`
//! with `gh` for PRs whose title has one of those keys, and looks closer
//! only at PRs that changed or whose checks are still running. What it saw
//! is kept in `watch.json` in drovr's data folder, so a poll after the app
//! was closed records what changed meanwhile. Changes go to the tickets'
//! records; the first poll only takes a baseline.

use crate::engine::Engine;
use crate::ledger;
use crate::shell::{self, quote};
use crate::sources::{adf_text, issue_fields, name_of, Sources};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// A ticket as the watcher last saw it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ticket {
    pub key: String,
    pub summary: String,
    pub status: String,
    /// Jira's status category: `new`, `indeterminate` or `done`.
    pub category: String,
    pub assignee: String,
    pub priority: String,
    pub issue_type: String,
    pub url: String,
    pub updated: String,
    pub comments: u64,
    /// "Author: the start of it"
    pub last_comment: String,
}

/// A pull request for one or more of the tickets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Pr {
    pub url: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub tickets: Vec<String>,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub draft: bool,
    /// `success`, `failure`, `pending`, or `none` when it has no checks.
    pub ci: String,
    /// The checks that failed, and where to read them.
    pub failing: Vec<String>,
    pub failing_urls: Vec<String>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty.
    pub review: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    pub head: String,
    pub updated: String,
    /// How many reviews it has had.
    pub reviews: u64,
    /// What `watch.rules` already acted on, as `trigger@head`.
    pub fired: Vec<String>,
}

/// When a source was last read, and what went wrong if it failed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Checked {
    pub at: f64,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub jira: Checked,
    pub github: Checked,
    pub tickets: BTreeMap<String, Ticket>,
    /// By URL.
    pub prs: BTreeMap<String, Pr>,
}

fn path() -> PathBuf {
    crate::config::home().join("watch.json")
}

/// What the watcher last saw.
pub fn load() -> State {
    load_from(&path())
}

fn load_from(p: &std::path::Path) -> State {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(state: &State, p: &std::path::Path) -> Result<()> {
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// One poll at a time, whether from the timer or Refresh now.
static POLLING: Mutex<()> = Mutex::new(());

/// Read what's due (everything, with `force`), record what changed, and
/// return what the watcher now knows. Errors are kept in the state.
pub fn poll(engine: &Engine, force: bool) -> State {
    poll_at(engine, force, &path())
}

fn poll_at(engine: &Engine, force: bool, file: &std::path::Path) -> State {
    let _one = POLLING.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load_from(file);
    let cfg = &engine.cfg.watch;
    let now = crate::store::now();
    if force || now - state.jira.at >= cfg.jira_poll_secs as f64 {
        poll_jira(engine, &mut state);
    }
    // Checks that are still running are worth a closer look.
    let running = state
        .prs
        .values()
        .any(|p| p.state == "OPEN" && p.ci == "pending");
    let every = if running {
        cfg.pr_poll_secs.min(60)
    } else {
        cfg.pr_poll_secs
    };
    if force || now - state.github.at >= every as f64 {
        poll_github(engine, &mut state);
    }
    if let Err(e) = save(&state, file) {
        eprintln!("drovr: couldn't save the watcher's state: {e:#}");
    }
    state
}

// -- Jira ---------------------------------------------------------------------

fn poll_jira(engine: &Engine, state: &mut State) {
    let sources = Sources::new(&engine.cfg);
    let baseline = state.jira.at == 0.0;
    state.jira.at = crate::store::now();
    let issues = match sources.jira_issues_raw(&engine.cfg.watch.jql) {
        Ok(v) => v,
        Err(e) => {
            state.jira.error = format!("{e:#}");
            return;
        }
    };
    state.jira.error.clear();
    // JSON that isn't a list of issues would otherwise read as an empty sprint.
    if !issues.is_empty()
        && issues
            .iter()
            .all(|i| i["key"].as_str().unwrap_or("").is_empty())
    {
        let fields: Vec<&str> = issues[0]
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        state.jira.error = format!(
            "jira.search_cmd printed JSON without issue keys (top-level fields: {}); it should print a list of issues, each with a \"key\"",
            fields.join(", ")
        );
        return;
    }
    let base_url = &engine.cfg.jira.base_url;
    let mut now = BTreeMap::new();
    for issue in &issues {
        let mut t = ticket_from(issue, base_url);
        if t.key.is_empty() {
            continue;
        }
        let before = state.tickets.get(&t.key);
        // Comments come only with a full read: for new and changed tickets.
        match before {
            Some(b) if b.updated == t.updated => {
                t.comments = b.comments;
                t.last_comment = b.last_comment.clone();
            }
            _ => match sources.jira_issue_raw(&t.key) {
                Ok(full) => (t.comments, t.last_comment) = comments_of(&full),
                Err(e) => state.jira.error = format!("{}: {e:#}", t.key),
            },
        }
        if !baseline {
            let changes = ticket_changes(before, &t);
            for (kind, data) in &changes {
                engine.record_ticket(&t.key, kind, data.clone());
            }
            if before.is_some() {
                crate::rules::on_ticket(engine, &t, &changes);
            }
        }
        now.insert(t.key.clone(), t);
    }
    if !baseline {
        for (key, old) in &state.tickets {
            if !now.contains_key(key) {
                let left = json!({ "status": old.status });
                engine.record_ticket(key, "sprint_left", left.clone());
                crate::rules::on_ticket(engine, old, &[("sprint_left", left)]);
            }
        }
    }
    state.tickets = now;
}

/// A ticket from Jira's REST shape or twg's.
pub fn ticket_from(issue: &Value, base_url: &str) -> Ticket {
    let f = issue_fields(issue);
    let key = issue["key"].as_str().unwrap_or("").to_uppercase();
    Ticket {
        url: match issue["url"].as_str() {
            Some(u) => u.to_string(),
            None if !base_url.is_empty() => {
                format!("{}/browse/{key}", base_url.trim_end_matches('/'))
            }
            None => String::new(),
        },
        summary: f["summary"].as_str().unwrap_or("").to_string(),
        status: name_of(&f["status"]),
        category: f["status"]["statusCategory"]["key"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        assignee: name_of(&f["assignee"]),
        priority: name_of(&f["priority"]),
        issue_type: match name_of(&f["issuetype"]) {
            t if t.is_empty() => name_of(&f["issueType"]),
            t => t,
        },
        updated: f["updated"].as_str().unwrap_or("").to_string(),
        key,
        ..Ticket::default()
    }
}

/// How many comments an issue has, and the latest one.
pub fn comments_of(issue: &Value) -> (u64, String) {
    let c = &issue_fields(issue)["comment"];
    let list = c["comments"].as_array().cloned().unwrap_or_default();
    let total = c["total"].as_u64().unwrap_or(list.len() as u64);
    let last = list
        .last()
        .map(|c| {
            let body = match &c["body"] {
                Value::String(s) => s.clone(),
                adf => adf_text(adf),
            };
            format!(
                "{}: {}",
                name_of(&c["author"]),
                crate::engine::truncate(body.trim(), 300)
            )
        })
        .unwrap_or_default();
    (total, last)
}

/// What changed on a ticket since the last poll, as record events.
pub fn ticket_changes(before: Option<&Ticket>, t: &Ticket) -> Vec<(&'static str, Value)> {
    let Some(b) = before else {
        return vec![(
            "sprint_joined",
            json!({ "status": t.status, "summary": t.summary }),
        )];
    };
    let mut out = vec![];
    if b.status != t.status {
        out.push((
            "jira_status",
            json!({ "from": b.status, "to": t.status, "category": t.category }),
        ));
    }
    if b.assignee != t.assignee {
        out.push((
            "jira_assignee",
            json!({ "from": b.assignee, "to": t.assignee }),
        ));
    }
    if t.comments > b.comments {
        out.push((
            "jira_comment",
            json!({ "new": t.comments - b.comments, "latest": t.last_comment }),
        ));
    }
    out
}

// -- GitHub -------------------------------------------------------------------

fn gh_json(cmd: &str) -> Result<Value> {
    let out = shell::sh(cmd, None, None, Duration::from_secs(60))?.into_stdout(cmd)?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("`{cmd}` didn't print JSON: {e}"))
}

fn poll_github(engine: &Engine, state: &mut State) {
    let cfg = &engine.cfg.watch;
    let baseline = state.github.at == 0.0;
    state.github.at = crate::store::now();
    if cfg.github_repos.is_empty() {
        state.github.error =
            "Set watch.github_repos (e.g. [\"yourorg/*\"]) to follow pull requests.".into();
        return;
    }
    let keys: BTreeSet<String> = state.tickets.keys().cloned().collect();
    let projects: BTreeSet<&str> = keys
        .iter()
        .filter_map(|k| k.rsplit_once('-').map(|(p, _)| p))
        .collect();
    let since = &ledger::iso(crate::store::now() - 30.0 * 86_400.0)[..10];
    let gh = quote(&cfg.gh);
    let mut errors = vec![];
    let mut found: BTreeMap<String, Pr> = BTreeMap::new();
    for scope in &cfg.github_repos {
        let filter = match scope.strip_suffix("/*") {
            Some(owner) => format!("--owner {}", quote(owner)),
            None => format!("--repo {}", quote(scope)),
        };
        // One search per project: the key's prefix, then keep exact keys.
        for project in &projects {
            let cmd = format!(
                "{gh} search prs {} --match title {filter} --updated {} --limit 100 \
                 --json number,title,url,repository,state,isDraft,updatedAt",
                quote(project),
                quote(&format!(">={since}"))
            );
            match gh_json(&cmd) {
                Ok(list) => {
                    for p in list.as_array().into_iter().flatten() {
                        let pr = pr_from_search(p, &keys);
                        if !pr.tickets.is_empty() {
                            found.insert(pr.url.clone(), pr);
                        }
                    }
                }
                Err(e) => errors.push(format!("{scope}: {e:#}")),
            }
        }
    }
    // Open PRs that dropped out of the search window are still followed.
    for (url, old) in &state.prs {
        if !found.contains_key(url)
            && old.state == "OPEN"
            && old.tickets.iter().any(|k| keys.contains(k))
        {
            found.insert(url.clone(), old.clone());
        }
    }
    let mut now = BTreeMap::new();
    for (url, mut pr) in found {
        let before = state.prs.get(&url);
        let look = match before {
            None => true,
            Some(b) => {
                // Checks finishing or re-running don't touch the PR's
                // updatedAt, so open PRs whose checks aren't green are
                // looked at every time.
                b.updated != pr.updated
                    || (b.state == "OPEN"
                        && (matches!(b.ci.as_str(), "pending" | "failure")
                            || b.mergeable == "UNKNOWN"))
            }
        };
        if look {
            let cmd = format!(
                "{gh} pr view {} --json state,isDraft,mergeable,reviewDecision,statusCheckRollup,headRefOid,updatedAt,title,reviews",
                quote(&url)
            );
            if let Some(b) = before {
                pr.fired = b.fired.clone();
            }
            match gh_json(&cmd) {
                Ok(v) => apply_view(&mut pr, &v),
                Err(e) => {
                    errors.push(format!("{url}: {e:#}"));
                    if let Some(b) = before {
                        pr = b.clone();
                    }
                }
            }
        } else if let Some(b) = before {
            pr = Pr {
                tickets: pr.tickets,
                ..b.clone()
            };
        }
        if !baseline {
            for (kind, data) in pr_changes(before, &pr) {
                for t in &pr.tickets {
                    engine.record_ticket(t, kind, data.clone());
                }
            }
            match before {
                Some(b) => crate::rules::on_pr(engine, b, &mut pr),
                // An open PR seen for the first time (new, or its ticket just
                // came into view): reviews, failing checks or a conflict
                // already on it are news too, once.
                None if pr.state == "OPEN" => {
                    let unseen = Pr {
                        state: "OPEN".into(),
                        ..Pr::default()
                    };
                    crate::rules::on_pr(engine, &unseen, &mut pr)
                }
                None => {}
            }
        }
        now.insert(url, pr);
    }
    state.prs = now;
    state.github.error = errors.join("\n");
}

/// A PR from `gh search prs`, with the tickets its title names.
pub fn pr_from_search(p: &Value, keys: &BTreeSet<String>) -> Pr {
    let title = p["title"].as_str().unwrap_or("").to_string();
    Pr {
        url: p["url"].as_str().unwrap_or("").to_string(),
        repo: p["repository"]["nameWithOwner"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        number: p["number"].as_u64().unwrap_or(0),
        tickets: ledger::tickets_in(&title)
            .into_iter()
            .filter(|k| keys.contains(k))
            .collect(),
        state: p["state"].as_str().unwrap_or("").to_uppercase(),
        draft: p["isDraft"] == true,
        updated: p["updatedAt"].as_str().unwrap_or("").to_string(),
        title,
        ..Pr::default()
    }
}

/// Fill in what `gh pr view` says.
pub fn apply_view(pr: &mut Pr, v: &Value) {
    pr.state = v["state"].as_str().unwrap_or(&pr.state).to_uppercase();
    pr.draft = v["isDraft"] == true;
    pr.mergeable = v["mergeable"].as_str().unwrap_or("").to_string();
    pr.review = v["reviewDecision"].as_str().unwrap_or("").to_string();
    pr.head = v["headRefOid"].as_str().unwrap_or("").to_string();
    if let Some(u) = v["updatedAt"].as_str() {
        pr.updated = u.to_string();
    }
    if let Some(t) = v["title"].as_str() {
        pr.title = t.to_string();
    }
    (pr.ci, pr.failing, pr.failing_urls) = checks(&v["statusCheckRollup"]);
    if let Some(r) = v["reviews"].as_array() {
        pr.reviews = r.len() as u64;
    }
}

/// The overall state of a PR's checks: check runs (status, conclusion) and
/// commit statuses (state).
pub fn checks(rollup: &Value) -> (String, Vec<String>, Vec<String>) {
    let list = rollup.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return ("none".into(), vec![], vec![]);
    }
    let (mut failing, mut urls, mut pending) = (vec![], vec![], false);
    for c in &list {
        let name = c["name"]
            .as_str()
            .or_else(|| c["context"].as_str())
            .unwrap_or("check")
            .to_string();
        let url = c["detailsUrl"]
            .as_str()
            .or_else(|| c["targetUrl"].as_str())
            .unwrap_or("")
            .to_string();
        let outcome = match c["conclusion"].as_str().filter(|s| !s.is_empty()) {
            Some(conclusion) => conclusion,
            None if c.get("status").is_some_and(|s| s != "COMPLETED") => "PENDING",
            None => c["state"].as_str().unwrap_or("PENDING"),
        };
        match outcome {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" | "WAITING" => pending = true,
            _ => {
                failing.push(name);
                urls.push(url);
            }
        }
    }
    let ci = if !failing.is_empty() && !pending {
        "failure"
    } else if pending {
        "pending"
    } else {
        "success"
    };
    (ci.into(), failing, urls)
}

/// What changed on a PR since the last poll, as record events.
pub fn pr_changes(before: Option<&Pr>, pr: &Pr) -> Vec<(&'static str, Value)> {
    let base = json!({ "pr": pr.url, "repo": pr.repo, "number": pr.number, "title": pr.title });
    let with = |extra: Value| {
        let mut v = base.clone();
        if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
            o.extend(e);
        }
        v
    };
    let Some(b) = before else {
        return vec![(
            "pr_linked",
            with(json!({ "state": pr.state, "ci": pr.ci, "review": pr.review, "draft": pr.draft })),
        )];
    };
    let mut changed = serde_json::Map::new();
    let mut note = |field: &str, from: Value, to: Value| {
        if from != to {
            changed.insert(field.into(), json!([from, to]));
        }
    };
    note("ci", json!(b.ci), json!(pr.ci));
    note("review", json!(b.review), json!(pr.review));
    note("mergeable", json!(b.mergeable), json!(pr.mergeable));
    note("draft", json!(b.draft), json!(pr.draft));
    note("head", json!(b.head), json!(pr.head));
    let mut out = vec![];
    if b.state != pr.state {
        match pr.state.as_str() {
            "MERGED" => out.push(("pr_merged", base.clone())),
            "CLOSED" => out.push(("pr_closed", base.clone())),
            _ => {
                changed.insert("state".into(), json!([b.state, pr.state]));
            }
        }
    }
    // A move to UNKNOWN is GitHub still working it out, not news.
    if changed.get("mergeable").is_some_and(|m| m[1] == "UNKNOWN") {
        changed.remove("mergeable");
    }
    if !changed.is_empty() {
        out.push((
            "pr_status",
            with(json!({ "changes": changed, "ci": pr.ci, "failing": pr.failing, "review": pr.review, "mergeable": pr.mergeable })),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;

    #[test]
    fn polls_records_changes_after_a_baseline() {
        let dir = std::env::temp_dir().join(format!("drovr-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.display();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        let sprint = |status: &str, updated: &str| {
            format!(
                r#"◇ Completed jira.workitem.query.
{{"issues":[{{"key":"SRE-3229","summary":"Kinds","status":{{"name":"{status}","statusCategory":{{"key":"indeterminate"}}}},"updated":"{updated}"}}]}}"#
            )
        };
        write("sprint.json", &sprint("To do", "u1"));
        write(
            "issue.json",
            r#"{"data":[{"key":"SRE-3229","comment":{"total":0,"comments":[]}}]}"#,
        );
        write(
            "search.json",
            r#"[{"number":7,"title":"feat(SRE-3229): kinds","url":"https://github.com/o/r/pull/7","repository":{"nameWithOwner":"o/r"},"state":"open","isDraft":false,"updatedAt":"p1"}]"#,
        );
        write(
            "view.json",
            r#"{"state":"OPEN","mergeable":"MERGEABLE","reviewDecision":"REVIEW_REQUIRED","headRefOid":"a1","statusCheckRollup":[{"name":"check","status":"IN_PROGRESS"}]}"#,
        );
        // A fake gh: search or view, from the files above.
        write("gh", &format!("#!/bin/sh\ncase \"$1\" in search) cat {d}/search.json;; pr) case \"$3\" in *pull/7) cat {d}/view.json;; *) cat {d}/view-$(basename \"$3\").json;; esac;; esac\n"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut cfg = Config::default();
        cfg.workflow.enabled = true;
        cfg.reports.dir = dir.join("plans").display().to_string();
        cfg.jira.provider = "command".into();
        cfg.jira.search_cmd = format!("cat {d}/sprint.json #{{query}}");
        cfg.jira.get_cmd = format!("cat {d}/issue.json #{{key}}");
        cfg.watch.github_repos = vec!["o/*".into()];
        cfg.watch.gh = dir.join("gh").display().to_string();
        let engine = Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into());
        engine
            .store
            .add_group("wif", "", "SRE-3229", "/tmp", "", false)
            .unwrap();
        let file = dir.join("watch.json");

        // The first poll takes a baseline and records nothing.
        let s = poll_at(&engine, true, &file);
        assert_eq!((s.jira.error.as_str(), s.github.error.as_str()), ("", ""));
        assert_eq!(s.tickets["SRE-3229"].status, "To do");
        assert_eq!(s.prs["https://github.com/o/r/pull/7"].ci, "pending");
        assert!(ledger::timeline(&engine.cfg, "SRE-3229").is_empty());

        // Then the ticket moves and CI fails.
        write("sprint.json", &sprint("In Progress", "u2"));
        write(
            "view.json",
            r#"{"state":"OPEN","mergeable":"MERGEABLE","reviewDecision":"REVIEW_REQUIRED","headRefOid":"a1","statusCheckRollup":[{"name":"check","status":"COMPLETED","conclusion":"FAILURE"}]}"#,
        );
        let s = poll_at(&engine, true, &file);
        assert_eq!(s.prs["https://github.com/o/r/pull/7"].failing, ["check"]);
        let events = ledger::timeline(&engine.cfg, "SRE-3229");
        let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        // Nobody's on the ticket, so watch.rules asks you about each.
        assert_eq!(kinds, ["jira_status", "asked", "pr_status", "asked"]);
        assert_eq!(events[0]["to"], "In Progress");
        assert_eq!(events[2]["changes"]["ci"], json!(["pending", "failure"]));
        assert_eq!(events[2]["source"], "watcher");

        // Nothing new, nothing recorded.
        poll_at(&engine, true, &file);
        assert_eq!(ledger::timeline(&engine.cfg, "SRE-3229").len(), 4);

        // Two PRs it hasn't seen before: a draft with reviews already on it
        // (CodeRabbit's, say), and one merged before drovr looked.
        let pr = |n: u32, state: &str, draft: bool| {
            format!(
                r#"{{"number":{n},"title":"fix(SRE-3229): {n}","url":"https://github.com/o/r/pull/{n}","repository":{{"nameWithOwner":"o/r"}},"state":"{state}","isDraft":{draft},"updatedAt":"p1"}}"#
            )
        };
        write(
            "search.json",
            &format!(
                "[{},{},{}]",
                pr(7, "open", false),
                pr(8, "open", true),
                pr(9, "merged", false)
            ),
        );
        write(
            "view-8.json",
            r#"{"state":"OPEN","isDraft":true,"mergeable":"MERGEABLE","reviewDecision":"","headRefOid":"b1","statusCheckRollup":[],"reviews":[{"author":{"login":"coderabbitai"},"state":"COMMENTED"},{"author":{"login":"coderabbitai"},"state":"COMMENTED"}]}"#,
        );
        write(
            "view-9.json",
            r#"{"state":"MERGED","headRefOid":"c1","statusCheckRollup":[]}"#,
        );
        poll_at(&engine, true, &file);
        let events = ledger::timeline(&engine.cfg, "SRE-3229");
        let new: Vec<String> = events[4..]
            .iter()
            .map(|e| {
                format!(
                    "{}{}",
                    e["kind"].as_str().unwrap(),
                    e["number"]
                        .as_u64()
                        .map(|n| format!(" #{n}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        // The draft's reviews are news; the old merge is only noted.
        assert_eq!(new, ["pr_linked #8", "asked", "pr_linked #9"]);
        assert!(
            events[5]["title"].as_str().unwrap_or("").contains("PR #8"),
            "{}",
            events[5]
        );
        // And only once.
        poll_at(&engine, true, &file);
        assert_eq!(ledger::timeline(&engine.cfg, "SRE-3229").len(), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_tickets_and_comments() {
        let issue: Value = serde_json::from_str(
            r#"{"key":"SRE-3229","summary":"Add repo kinds","status":{"id":"3","name":"In Progress","statusCategory":{"key":"indeterminate"}},
                "issueType":{"name":"Task"},"priority":{"name":"Medium/P3"},"assignee":{"displayName":"Izz Noland"},
                "updated":"2026-10-26T17:10:43.476-0400","url":"https://yourco.atlassian.net/browse/SRE-3229"}"#,
        )
        .unwrap();
        let t = ticket_from(&issue, "");
        assert_eq!(
            (
                t.key.as_str(),
                t.status.as_str(),
                t.category.as_str(),
                t.issue_type.as_str()
            ),
            ("SRE-3229", "In Progress", "indeterminate", "Task")
        );
        let full = json!({ "comment": { "total": 2, "comments": [
            { "author": { "displayName": "A" }, "body": "first" },
            { "author": { "displayName": "Parker" }, "body": { "type": "doc", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "Ship it" }] }] } }
        ] } });
        assert_eq!(comments_of(&full), (2, "Parker: Ship it".to_string()));

        let mut moved = t.clone();
        moved.status = "Complete".into();
        moved.comments = 2;
        moved.last_comment = "Parker: Ship it".into();
        let kinds: Vec<&str> = ticket_changes(Some(&t), &moved)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(kinds, ["jira_status", "jira_comment"]);
        assert_eq!(ticket_changes(None, &t)[0].0, "sprint_joined");
        assert!(ticket_changes(Some(&t), &t).is_empty());
    }

    #[test]
    fn reads_checks() {
        let rollup = json!([
            { "__typename": "CheckRun", "name": "check", "status": "COMPLETED", "conclusion": "SUCCESS" },
            { "__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SKIPPED" },
            { "__typename": "StatusContext", "context": "ci/legacy", "state": "SUCCESS" }
        ]);
        assert_eq!(checks(&rollup).0, "success");
        let failed = json!([
            { "name": "check", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2" },
            { "name": "e2e", "status": "COMPLETED", "conclusion": "SUCCESS" }
        ]);
        let (ci, failing, urls) = checks(&failed);
        assert_eq!(
            (ci.as_str(), failing, urls),
            (
                "failure",
                vec!["check".to_string()],
                vec!["https://github.com/o/r/actions/runs/1/job/2".to_string()]
            )
        );
        // A failure while others still run isn't final yet.
        let running = json!([
            { "name": "check", "status": "COMPLETED", "conclusion": "FAILURE" },
            { "name": "e2e", "status": "IN_PROGRESS", "conclusion": "" }
        ]);
        assert_eq!(checks(&running).0, "pending");
        assert_eq!(checks(&json!([])).0, "none");
        assert_eq!(
            checks(&json!([{ "context": "deploy", "state": "PENDING" }])).0,
            "pending"
        );
    }

    #[test]
    fn links_prs_and_spots_changes() {
        let keys: BTreeSet<String> = ["SRE-3229".to_string()].into();
        let p = json!({ "number": 4412, "title": "feat(SRE-3229): kinds in acme-github", "url": "https://github.com/o/r/pull/4412",
                        "repository": { "nameWithOwner": "o/r" }, "state": "open", "isDraft": false, "updatedAt": "t1" });
        let pr = pr_from_search(&p, &keys);
        assert_eq!(
            (pr.tickets.clone(), pr.state.as_str(), pr.repo.as_str()),
            (vec!["SRE-3229".to_string()], "OPEN", "o/r")
        );
        assert!(pr_from_search(&json!({ "title": "fix SRE-1" }), &keys)
            .tickets
            .is_empty());

        let mut a = pr.clone();
        apply_view(
            &mut a,
            &json!({ "state": "OPEN", "mergeable": "MERGEABLE", "reviewDecision": "REVIEW_REQUIRED", "headRefOid": "abc",
            "statusCheckRollup": [{ "name": "check", "status": "IN_PROGRESS" }] }),
        );
        assert_eq!(pr_changes(None, &a)[0].0, "pr_linked");
        let mut b = a.clone();
        apply_view(
            &mut b,
            &json!({ "state": "OPEN", "mergeable": "UNKNOWN", "reviewDecision": "CHANGES_REQUESTED", "headRefOid": "abc",
            "statusCheckRollup": [{ "name": "check", "status": "COMPLETED", "conclusion": "FAILURE" }] }),
        );
        let changes = pr_changes(Some(&a), &b);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "pr_status");
        assert_eq!(
            changes[0].1["changes"],
            json!({ "ci": ["pending", "failure"], "review": ["REVIEW_REQUIRED", "CHANGES_REQUESTED"] })
        );
        let mut merged = b.clone();
        merged.state = "MERGED".into();
        assert_eq!(pr_changes(Some(&b), &merged)[0].0, "pr_merged");
        assert!(pr_changes(Some(&b), &b).is_empty());
    }
}
