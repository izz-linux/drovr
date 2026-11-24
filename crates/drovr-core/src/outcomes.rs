//! What became of each PR: who built it, how much rework it took and how
//! long it was before it merged. The Models screen compares models on it.

use crate::config::reviewer_name;
use crate::engine::Engine;
use crate::insights::model_label;
use crate::review;
use crate::spend::records;
use crate::watch::{pr_changes, Pr};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const BUILD: &str = "build";

/// Fewer PRs than this is too few to compare.
pub const ENOUGH: usize = 10;

/// A PR first looked at this long after it was opened may have a history
/// the watcher didn't see.
const SEEN_LATE: f64 = 3600.0;

/// A PR, from when the watcher first saw it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outcome {
    pub url: String,
    pub repo: String,
    pub number: u64,
    pub tickets: Vec<String>,
    /// The build agent whose branch it is; empty when that isn't certain.
    pub builder: String,
    pub state: String,
    pub draft: bool,
    /// When it was opened, when known.
    pub opened: Option<f64>,
    /// When it was first out of draft, when known.
    pub ready: Option<f64>,
    pub closed: Option<f64>,
    /// Heads that people or bots other than its author reviewed.
    pub rounds: Option<u64>,
    /// Pushes once it was ready for review.
    pub pushes: u64,
    pub ci_failures: u64,
    /// The first push, failed CI or changes requested once it was ready.
    pub rework: Option<f64>,
    /// Added and deleted.
    pub lines: Option<u64>,
    /// The watcher saw it from the start.
    pub whole: bool,
}

/// How PRs of one size, built by one model, turned out.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelRow {
    /// `small` (up to 100 lines), `medium` (up to 500), `large` or `unknown`.
    pub size: String,
    pub model: String,
    /// Its builders had a reviewer.
    pub reviewed: bool,
    pub prs: usize,
    /// `ENOUGH` PRs to compare.
    pub enough: bool,
    /// A PR's average cost: before rework, and in it.
    pub first: f64,
    pub rework: f64,
    /// Runs whose CLI reported no cost, which the averages leave out.
    pub unpriced: u64,
    /// PRs that took rework.
    pub reworked: usize,
    pub hours: Option<f64>,
    pub rounds: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Models {
    pub rows: Vec<ModelRow>,
    /// PRs merged, those seen whole, and how many of those took rework.
    pub merged: usize,
    pub whole: usize,
    pub reworked: usize,
    /// The median from ready to merged.
    pub hours: Option<f64>,
    /// Merged PRs the rows leave out, by why.
    pub left_out: BTreeMap<String, usize>,
}

/// Seconds since 1970 for GitHub's `2026-11-24T11:21:44Z`.
pub fn parse_iso(s: &str) -> Option<f64> {
    let (date, time) = s.strip_suffix('Z')?.split_once('T')?;
    let three = |s: &str, by: char| -> Option<[i64; 3]> {
        let n: Vec<i64> = s.split(by).map(|p| p.parse().ok()).collect::<Option<_>>()?;
        n.try_into().ok()
    };
    let [y, m, d] = three(date, '-')?;
    let [hour, min, sec] = three(time, ':')?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970 in the Gregorian calendar, with years from March.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let year = y.rem_euclid(400);
    let day = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + year * 365 + year / 4 - year / 100 + day - 719_468;
    Some((days * 86_400 + hour * 3600 + min * 60 + sec) as f64)
}

impl Outcome {
    /// Add what a record event (`pr_changes`) says happened at `ts`.
    fn apply(&mut self, kind: &str, data: &Value, ts: f64) {
        match kind {
            "pr_linked" => {
                self.state = data["state"].as_str().unwrap_or("").into();
                self.draft = data["draft"] == true;
                self.opened.get_or_insert(ts);
                if self.state == "OPEN" && !self.draft {
                    self.ready = Some(ts);
                }
            }
            "pr_merged" => {
                self.state = "MERGED".into();
                self.closed = Some(ts);
            }
            "pr_closed" => {
                self.state = "CLOSED".into();
                self.closed = Some(ts);
            }
            "pr_status" => {
                let c = &data["changes"];
                if c["state"][1] == "OPEN" {
                    self.state = "OPEN".into();
                    self.closed = None;
                }
                // What came along with leaving draft was still building.
                let waiting = self.state == "OPEN" && !self.draft && self.ready.is_some();
                let pushed = c["head"][0].as_str().is_some_and(|h| !h.is_empty());
                let failed = c["ci"][1] == "failure" && self.state == "OPEN";
                let asked = c["review"][1] == "CHANGES_REQUESTED";
                self.ci_failures += u64::from(failed);
                if waiting {
                    self.pushes += u64::from(pushed);
                    if pushed || failed || asked {
                        self.rework.get_or_insert(ts);
                    }
                }
                if let Some(draft) = c["draft"][1].as_bool() {
                    self.draft = draft;
                    if !draft {
                        self.ready.get_or_insert(ts);
                    }
                }
            }
            _ => {}
        }
    }

    fn size(&self) -> &'static str {
        match self.lines {
            None => "unknown",
            Some(0..=100) => "small",
            Some(101..=500) => "medium",
            Some(_) => "large",
        }
    }

    fn on(&self, tickets: &str) -> bool {
        tickets
            .split(',')
            .any(|t| self.tickets.iter().any(|k| k == t))
    }
}

/// Whether a remote's URL is `owner/name`.
fn is_repo(remote: &str, repo: &str) -> bool {
    let remote = remote.trim_end_matches('/').to_lowercase();
    let remote = remote.strip_suffix(".git").unwrap_or(&remote);
    let repo = repo.to_lowercase();
    !repo.is_empty()
        && (remote.ends_with(&format!("/{repo}")) || remote.ends_with(&format!(":{repo}")))
}

fn median(mut of: Vec<f64>) -> Option<f64> {
    of.sort_by(f64::total_cmp);
    let mid = of.len() / 2;
    match of.len() {
        0 => None,
        n if n % 2 == 1 => Some(of[mid]),
        _ => Some((of[mid - 1] + of[mid]) / 2.0),
    }
}

/// Whether PRs can be compared under a model's label.
fn comparable(model: &str) -> bool {
    let name = model.trim_end_matches(" (OC)");
    !name.ends_with("(routed)")
        && !name.ends_with("(version unknown)")
        && !matches!(name, "unknown" | "default" | "several models")
}

impl Engine {
    /// Keep up with a PR the watcher just read. A failure is only printed.
    pub(crate) fn observe_pr(&self, before: Option<&Pr>, pr: &Pr, baseline: bool) {
        if let Err(e) = self.try_observe_pr(before, pr, baseline) {
            eprintln!("drovr: couldn't log what became of {}: {e:#}", pr.url);
        }
    }

    fn try_observe_pr(&self, before: Option<&Pr>, pr: &Pr, baseline: bool) -> Result<()> {
        let now = crate::store::now();
        let known = self.store.outcome(&pr.url)?;
        let mut o = known.clone().unwrap_or_default();
        let seen_closed = o.closed;
        o.url = pr.url.clone();
        o.repo = pr.repo.clone();
        o.number = pr.number;
        o.tickets = pr.tickets.clone();
        match (&known, before) {
            (Some(_), Some(b)) => {
                for (kind, data) in pr_changes(Some(b), pr) {
                    o.apply(kind, &data, now);
                }
            }
            // First seen, or back after it wasn't followed for a while.
            _ => {
                let new = known.is_none() && before.is_none() && !baseline;
                o.whole =
                    new && pr.state == "OPEN" && pr.created > 0.0 && now - pr.created < SEEN_LATE;
                o.state = pr.state.clone();
                o.draft = pr.draft;
                if o.whole && !pr.draft {
                    o.ready = Some(pr.created);
                }
            }
        }
        if o.state != "OPEN" {
            o.closed = match pr.closed {
                at if at > 0.0 => Some(at),
                _ => seen_closed.or(Some(now)),
            };
        }
        if pr.created > 0.0 {
            o.opened = Some(pr.created);
        }
        if !pr.head.is_empty() {
            o.rounds = Some(pr.review_rounds);
            o.lines = Some(pr.lines);
        }
        let moved = before
            .is_none_or(|b| b.head != pr.head || b.state != pr.state || b.branch != pr.branch);
        if o.builder.is_empty() && moved {
            o.builder = self.builder_of(pr).unwrap_or_default();
        }
        if known.as_ref() != Some(&o) {
            self.store.save_outcome(&o)?;
        }
        Ok(())
    }

    /// The one build agent on the PR's ticket that works on its branch.
    fn builder_of(&self, pr: &Pr) -> Option<String> {
        if pr.branch.is_empty() {
            return None;
        }
        if let Some(name) = pr.branch.strip_prefix("drovr/") {
            return self.store.agent(name).ok()?.map(|a| a.name);
        }
        let mut on: Vec<String> = self
            .store
            .agents(None)
            .ok()?
            .into_iter()
            .filter(|a| a.stage == BUILD)
            .filter(|a| {
                let (_, keys) = self.ticket_place(a);
                keys.iter().any(|k| pr.tickets.contains(k))
            })
            .filter(|a| {
                review::roots(&a.cwd, &self.transcripts(a))
                    .iter()
                    .filter_map(|root| review::checked_out(root))
                    .any(|(branch, remote)| branch == pr.branch && is_repo(&remote, &pr.repo))
            })
            .map(|a| a.name)
            .collect();
        (on.len() == 1).then(|| on.remove(0))
    }

    /// Add the PRs in the tickets' records from before outcomes were kept.
    /// Once per database; returns how many it added.
    pub fn import_outcomes(&self) -> Result<usize> {
        const ONCE: &str = "import outcomes";
        if self.store.done_once(ONCE)? {
            return Ok(0);
        }
        let mut prs: BTreeMap<String, Vec<(f64, String, Value)>> = BTreeMap::new();
        let mut sent: Vec<(f64, String)> = vec![];
        for (ticket, events) in records(&self.cfg) {
            for mut e in events {
                let ts = e["ts"].as_f64().unwrap_or(0.0);
                let kind = e["kind"].as_str().unwrap_or("").to_string();
                if kind == "handoff_sent_back" && e["stage"] == BUILD {
                    sent.push((ts, e["agent"].as_str().unwrap_or("").into()));
                }
                let url = e["pr"].as_str().unwrap_or("").to_string();
                if kind.starts_with("pr_") && !url.is_empty() {
                    if e["ticket"].is_null() {
                        e["ticket"] = ticket.clone().into();
                    }
                    prs.entry(url).or_default().push((ts, kind, e));
                }
            }
        }
        // What's on several tickets is in each one's record, a moment apart.
        let same = |a: f64, b: f64| (a - b).abs() < 2.0;
        sent.sort_by(|a, b| a.0.total_cmp(&b.0));
        sent.dedup_by(|b, a| a.1 == b.1 && same(a.0, b.0));
        let mut out = vec![];
        for (url, mut events) in prs {
            events.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut o = Outcome {
                url,
                whole: events[0].1 == "pr_linked" && events[0].2["state"] == "OPEN",
                ..Outcome::default()
            };
            let mut tickets = BTreeSet::new();
            let mut done: Vec<(f64, String, Value)> = vec![];
            for (ts, kind, e) in events {
                o.repo = e["repo"].as_str().unwrap_or("").into();
                o.number = e["number"].as_u64().unwrap_or(0);
                tickets.insert(e["ticket"].as_str().unwrap_or("").to_string());
                let what = e["changes"].clone();
                let again = |d: &(f64, String, Value)| same(d.0, ts) && d.1 == kind && d.2 == what;
                // Linked again after it wasn't followed for a while.
                let relinked = kind == "pr_linked" && !done.is_empty();
                if done.iter().any(again) {
                    continue;
                }
                if relinked {
                    o.whole = false;
                }
                o.apply(&kind, &e, ts);
                done.push((ts, kind, what));
            }
            o.tickets = tickets.into_iter().collect();
            out.push(o);
        }
        self.store.import_outcomes(ONCE, &out, &sent)
    }

    /// How PRs merged from `from` up to `to` turned out, by model.
    ///
    /// A run is shared evenly by its builder's PRs that hadn't merged or
    /// closed yet when it started. On those that took rework, it's rework if
    /// they were ready, or their handoff sent back, by then, whichever model
    /// ran it.
    pub fn models(&self, from: f64, to: f64) -> Result<Models> {
        let mut prs = self.store.outcomes()?;
        let spent = self.store.spend_since(0.0)?;
        let sent = self.store.send_backs()?;
        for o in prs.iter_mut().filter(|o| o.builder.is_empty()) {
            let on: BTreeSet<&str> = spent
                .iter()
                .filter(|r| r.stage == BUILD && o.on(&r.tickets))
                .map(|r| r.agent.as_str())
                .collect();
            if let (1, Some(only)) = (on.len(), on.first()) {
                o.builder = only.to_string();
            }
        }
        // Runs that started from here on are rework. A push is only seen
        // after the run that made it, so that's once the PR was ready.
        for o in &mut prs {
            let back = sent
                .iter()
                .filter(|(_, agent)| *agent == o.builder)
                .filter(|(ts, _)| o.opened.is_some_and(|at| *ts >= at))
                .find(|(ts, _)| o.closed.is_none_or(|c| *ts < c))
                .map(|(ts, _)| *ts);
            let ready = o.rework.and(o.ready);
            o.rework = match (ready, back) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        // A run's rows share its time; only its main model's has how long.
        let mut starts: BTreeMap<(&str, i64, u64), f64> = BTreeMap::new();
        for r in spent.iter().filter(|r| r.main) {
            let took = r.duration_ms.unwrap_or(0) as f64 / 1000.0;
            starts.insert((&r.agent, r.run, r.ts.to_bits()), r.ts - took);
        }

        let mut out = Models::default();
        let mut groups: BTreeMap<(usize, String, bool), Vec<Built>> = BTreeMap::new();
        for o in &prs {
            let Some(closed) = o.closed.filter(|_| o.state == "MERGED") else {
                continue;
            };
            if closed < from || closed >= to {
                continue;
            }
            out.merged += 1;
            let mut leave = |why: &str| *out.left_out.entry(why.into()).or_default() += 1;
            if !o.whole {
                leave("not seen from the start");
                continue;
            }
            out.whole += 1;
            out.reworked += usize::from(o.rework.is_some());
            if o.builder.is_empty() {
                leave("no one builder is certain");
                continue;
            }
            let reviewer = reviewer_name(&o.builder);
            let mut built = Built {
                hours: o.ready.map(|r| (closed - r).max(0.0) / 3600.0),
                rounds: o.rounds.map(|n| n as f64),
                reworked: o.rework.is_some(),
                ..Built::default()
            };
            let (mut models, mut reviewed): (BTreeMap<String, usize>, bool) = Default::default();
            for r in &spent {
                if (r.agent != o.builder && r.agent != reviewer) || !o.on(&r.tickets) {
                    continue;
                }
                let start = starts
                    .get(&(r.agent.as_str(), r.run, r.ts.to_bits()))
                    .copied()
                    .unwrap_or(r.ts);
                if start >= closed {
                    continue;
                }
                let share = prs
                    .iter()
                    .filter(|p| p.builder == o.builder && p.on(&r.tickets))
                    .filter(|p| p.closed.is_none_or(|c| c > start))
                    .count();
                let usd = r.cost_usd.unwrap_or(0.0) / share as f64;
                let again = o.rework.is_some_and(|at| start >= at);
                *(if again {
                    &mut built.rework
                } else {
                    &mut built.first
                }) += usd;
                built.unpriced += u64::from(r.main && r.cost_usd.is_none());
                if r.agent == reviewer {
                    reviewed = true;
                } else if r.main && !again {
                    *models.entry(model_label(r)).or_default() += 1;
                }
            }
            let runs: usize = models.values().sum();
            let Some((model, n)) = models.into_iter().max_by_key(|(_, n)| *n) else {
                leave("no runs logged");
                continue;
            };
            if n * 5 < runs * 4 || !comparable(&model) {
                leave("several models, or an unknown one");
                continue;
            }
            let size = SIZES.iter().position(|s| *s == o.size()).unwrap_or(0);
            groups
                .entry((size, model, reviewed))
                .or_default()
                .push(built);
        }
        out.hours = median(
            prs.iter()
                .filter(|o| o.state == "MERGED" && o.whole)
                .filter_map(|o| Some((o.closed?, o.ready?)))
                .filter(|(closed, _)| *closed >= from && *closed < to)
                .map(|(closed, ready)| (closed - ready).max(0.0) / 3600.0)
                .collect(),
        );
        for ((size, model, reviewed), of) in groups {
            let n = of.len();
            let average = |f: fn(&Built) -> f64| of.iter().map(f).sum::<f64>() / n as f64;
            out.rows.push(ModelRow {
                size: SIZES[size].into(),
                model,
                reviewed,
                prs: n,
                enough: n >= ENOUGH,
                first: average(|b| b.first),
                rework: average(|b| b.rework),
                unpriced: of.iter().map(|b| b.unpriced).sum(),
                reworked: of.iter().filter(|b| b.reworked).count(),
                hours: median(of.iter().filter_map(|b| b.hours).collect()),
                rounds: median(of.iter().filter_map(|b| b.rounds).collect()),
            });
        }
        out.rows.sort_by_key(|r| {
            let size = SIZES.iter().position(|s| *s == r.size);
            (size, std::cmp::Reverse(r.prs))
        });
        Ok(out)
    }
}

const SIZES: [&str; 4] = ["small", "medium", "large", "unknown"];

/// What one PR took to build.
#[derive(Default)]
struct Built {
    first: f64,
    rework: f64,
    unpriced: u64,
    reworked: bool,
    hours: Option<f64>,
    rounds: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::ledger;
    use crate::spend::Row;
    use crate::store::{Agent, Store};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn engine(case: &str) -> (Engine, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("drovr-outcomes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.workflow.enabled = true;
        cfg.reports.dir = dir.join("plans").display().to_string();
        let store = Store::open(&dir.join("t.db")).unwrap();
        (Engine::with(store, cfg, "drovr".into()), dir)
    }

    fn status(changes: Value) -> (&'static str, Value) {
        ("pr_status", json!({ "changes": changes }))
    }

    #[test]
    fn reads_githubs_times() {
        for (s, want) in [
            ("1970-01-01T00:00:00Z", Some(0.0)),
            ("2000-03-01T00:00:01Z", Some(951868801.0)),
            ("2024-02-29T12:00:00Z", Some(1709208000.0)),
            ("", None),
            ("yesterday", None),
            ("2024-13-01T00:00:00Z", None),
        ] {
            assert_eq!(parse_iso(s), want, "{s}");
        }
    }

    #[test]
    fn rework_starts_once_a_pr_is_ready() {
        let linked = |draft: bool| ("pr_linked", json!({ "state": "OPEN", "draft": draft }));
        let push = || status(json!({ "head": ["a", "b"] }));
        let ready = || status(json!({ "draft": [true, false] }));
        let failed = || status(json!({ "ci": ["pending", "failure"] }));
        let asked = || status(json!({ "review": ["", "CHANGES_REQUESTED"] }));
        // (ready, pushes, CI failures, rework), one event a second from 1.
        for (case, events, want) in [
            (
                "a draft's pushes and failures are building",
                vec![linked(true), push(), failed()],
                (None, 0, 1, None),
            ),
            (
                "a push once ready",
                vec![linked(true), ready(), push(), push()],
                (Some(2.0), 2, 0, Some(3.0)),
            ),
            (
                "opened ready, CI fails",
                vec![linked(false), failed()],
                (Some(1.0), 0, 1, Some(2.0)),
            ),
            (
                "changes requested",
                vec![linked(false), asked()],
                (Some(1.0), 0, 0, Some(2.0)),
            ),
            (
                "pushed and marked ready at once",
                vec![
                    linked(true),
                    status(
                        json!({ "draft": [true, false], "head": ["a", "b"], "ci": ["pending", "failure"] }),
                    ),
                ],
                (Some(2.0), 0, 1, None),
            ),
            (
                "back in draft",
                vec![
                    linked(false),
                    status(json!({ "draft": [false, true] })),
                    push(),
                ],
                (Some(1.0), 0, 0, None),
            ),
            (
                "its head read for the first time",
                vec![linked(false), status(json!({ "head": ["", "b"] }))],
                (Some(1.0), 0, 0, None),
            ),
            (
                "after the merge",
                vec![linked(false), ("pr_merged", json!({})), push(), failed()],
                (Some(1.0), 0, 0, None),
            ),
        ] {
            let mut o = Outcome::default();
            for (i, (kind, data)) in events.iter().enumerate() {
                o.apply(kind, data, i as f64 + 1.0);
            }
            assert_eq!((o.ready, o.pushes, o.ci_failures, o.rework), want, "{case}");
        }
    }

    #[test]
    fn closing_and_reopening() {
        let mut o = Outcome::default();
        o.apply(
            "pr_linked",
            &json!({ "state": "OPEN", "draft": false }),
            1.0,
        );
        o.apply("pr_closed", &json!({}), 2.0);
        assert_eq!((o.state.as_str(), o.closed), ("CLOSED", Some(2.0)));
        o.apply(
            "pr_status",
            &json!({ "changes": { "state": ["CLOSED", "OPEN"] } }),
            3.0,
        );
        assert_eq!((o.state.as_str(), o.closed), ("OPEN", None));
        o.apply("pr_merged", &json!({}), 4.0);
        assert_eq!((o.state.as_str(), o.closed), ("MERGED", Some(4.0)));
    }

    fn pr(created: f64) -> Pr {
        Pr {
            url: "u/1".into(),
            repo: "o/r".into(),
            number: 1,
            tickets: vec!["SRE-1".into()],
            state: "OPEN".into(),
            ci: "success".into(),
            head: "a".into(),
            created,
            review_rounds: 1,
            lines: 40,
            ..Pr::default()
        }
    }

    #[test]
    fn follows_a_pr_from_the_start() {
        let (e, dir) = engine("follow");
        let now = crate::store::now();
        let first = pr(now - 60.0);
        e.observe_pr(None, &first, false);
        let o = &e.store.outcomes().unwrap()[0];
        assert!(o.whole);
        assert_eq!(
            (o.ready, o.rounds, o.lines),
            (Some(now - 60.0), Some(1), Some(40))
        );

        let mut merged = first.clone();
        merged.state = "MERGED".into();
        merged.closed = now - 5.0;
        merged.review_rounds = 3;
        e.observe_pr(Some(&first), &merged, false);
        let got = e.store.outcomes().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].state.as_str(), got[0].closed, got[0].rounds),
            ("MERGED", Some(now - 5.0), Some(3))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_merge_keeps_when_it_was_first_seen() {
        let (e, dir) = engine("closed-once");
        let mut p = pr(crate::store::now());
        e.observe_pr(None, &p, false);
        let open = p.clone();
        p.state = "MERGED".into();
        e.observe_pr(Some(&open), &p, false);
        let first = e.store.outcomes().unwrap()[0].closed;
        assert!(first.is_some());
        std::thread::sleep(std::time::Duration::from_millis(20));
        e.observe_pr(Some(&p.clone()), &p, false);
        assert_eq!(e.store.outcomes().unwrap()[0].closed, first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pr_seen_late_is_not_whole() {
        let now = crate::store::now();
        for (case, created, before, baseline) in [
            ("late", now - 7200.0, false, false),
            ("unknown", 0.0, false, false),
            ("baseline", now - 60.0, false, true),
            ("midway", now - 60.0, true, false),
        ] {
            let (e, dir) = engine(&format!("seen-{case}"));
            let (p, old) = (pr(created), pr(created));
            e.observe_pr(before.then_some(&old), &p, baseline);
            let got = e.store.outcomes().unwrap();
            assert_eq!(got.len(), 1, "{case}");
            assert!(!got[0].whole, "{case}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn outcomes_that_cannot_be_written_are_only_reported() {
        let (e, dir) = engine("unwritable");
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute_batch("DROP TABLE pr_outcomes")
            .unwrap();
        e.observe_pr(None, &pr(1.0), false);
        assert!(e.store.outcomes().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&ok.stderr)
        );
    }

    /// A repo of `o/r` on `branch`.
    fn repo(dir: &Path, name: &str, remote: &str, branch: &str) -> String {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        git(&d, &["init", "-q", "-b", branch]);
        git(&d, &["remote", "add", "origin", remote]);
        git(&d, &["commit", "-q", "--allow-empty", "-m", "x"]);
        d.display().to_string()
    }

    fn add(e: &Engine, name: &str, stage: &str, cwd: &str) {
        e.store
            .add_agent(&Agent {
                name: format!("{name}-{}", std::process::id()),
                role: "worker".into(),
                backend: "headless".into(),
                harness: "claude".into(),
                run_state: "idle".into(),
                ticket: "SRE-1".into(),
                stage: stage.into(),
                cwd: cwd.into(),
                ..Agent::default()
            })
            .unwrap();
    }

    #[test]
    fn a_pr_is_its_branchs_build_agents() {
        let named = |n: &str| Some(format!("{n}-{}", std::process::id()));
        let ssh = "git@github.com:O/r.git";
        let https = "https://github.com/o/r";
        for (case, agents, branch, want) in [
            (
                "its worktree",
                vec![
                    ("oa", "build", ssh, "feat"),
                    ("ob", "build", https, "other"),
                ],
                "feat",
                named("oa"),
            ),
            (
                "its reviewer isn't the builder",
                vec![
                    ("oc", "build", https, "feat"),
                    ("oc-review", "review", https, "feat"),
                ],
                "feat",
                named("oc"),
            ),
            (
                "two on one branch",
                vec![("od", "build", ssh, "feat"), ("oe", "build", ssh, "feat")],
                "feat",
                None,
            ),
            (
                "the same branch in another repo",
                vec![("of", "build", "https://github.com/o/r2", "feat")],
                "feat",
                None,
            ),
            (
                "nobody's branch",
                vec![("og", "build", ssh, "feat")],
                "mine",
                None,
            ),
            (
                "no branch known",
                vec![("oh", "build", ssh, "feat")],
                "",
                None,
            ),
        ] {
            let (e, dir) = engine(&format!("who-{}", case.replace([' ', '\''], "-")));
            for (name, stage, remote, on) in &agents {
                add(&e, name, stage, &repo(&dir, name, remote, on));
            }
            let mut p = pr(0.0);
            p.branch = branch.into();
            assert_eq!(e.builder_of(&p), want, "{case}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn drovrs_own_branch_names_its_agent() {
        let (e, dir) = engine("who-drovr");
        add(&e, "oi", "build", "/nonexistent");
        let name = format!("oi-{}", std::process::id());
        let mut p = pr(0.0);
        p.branch = format!("drovr/{name}");
        assert_eq!(e.builder_of(&p), Some(name));
        p.branch = "drovr/someone-else".into();
        assert_eq!(e.builder_of(&p), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_builder_once_known_is_kept() {
        let (e, dir) = engine("kept");
        add(
            &e,
            "oj",
            "build",
            &repo(&dir, "oj", "git@github.com:o/r.git", "feat"),
        );
        let mut p = pr(crate::store::now());
        p.branch = "feat".into();
        e.observe_pr(None, &p, false);
        let name = format!("oj-{}", std::process::id());
        assert_eq!(e.store.outcomes().unwrap()[0].builder, name);
        e.store.remove_agent(&name).unwrap();
        let mut pushed = p.clone();
        pushed.head = "b".into();
        e.observe_pr(Some(&p), &pushed, false);
        let o = &e.store.outcomes().unwrap()[0];
        assert_eq!((o.builder.as_str(), o.pushes), (name.as_str(), 1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imports_recorded_prs_once() {
        let (e, dir) = engine("import");
        let ev = |ts: f64, kind: &str, more: Value| {
            let mut v = json!({ "ts": ts, "kind": kind, "pr": "u/1", "repo": "o/r", "number": 1 });
            v.as_object_mut()
                .unwrap()
                .extend(more.as_object().unwrap().clone());
            v.to_string()
        };
        let write = |ticket: &str, late: f64| {
            let d = ledger::dir(&e.cfg, "g", ticket);
            std::fs::create_dir_all(&d).unwrap();
            let lines = [
                ev(100.0 + late, "pr_linked", json!({ "state": "OPEN", "draft": false })),
                ev(200.0 + late, "pr_status", json!({ "changes": { "head": ["a", "b"] } })),
                ev(300.0 + late, "pr_merged", json!({})),
                json!({ "ts": 150.0 + late, "kind": "handoff_sent_back", "agent": "b", "stage": "build" }).to_string(),
            ];
            std::fs::write(d.join(ledger::FILE), lines.join("\n")).unwrap();
        };
        // The same PR in two tickets' records, a moment apart.
        write("SRE-1", 0.0);
        write("SRE-2", 0.02);
        assert_eq!(e.import_outcomes().unwrap(), 1);
        let o = &e.store.outcomes().unwrap()[0];
        assert_eq!(
            (o.pushes, o.ready, o.closed, o.rework),
            (1, Some(100.0), Some(300.0), Some(200.0))
        );
        assert_eq!(o.tickets, ["SRE-1", "SRE-2"]);
        assert!(o.whole);
        assert_eq!(o.rounds, None);
        assert_eq!(e.store.send_backs().unwrap(), [(150.0, "b".to_string())]);
        assert_eq!(e.import_outcomes().unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn run(agent: &str, n: i64, ts: f64, model: &str, cost: Option<f64>) -> Row {
        Row {
            ts,
            agent: agent.into(),
            tickets: "SRE-1".into(),
            stage: if agent.ends_with("-review") {
                "review"
            } else {
                "build"
            }
            .into(),
            harness: "claude".into(),
            model: model.into(),
            run: n,
            main: true,
            cost_usd: cost,
            duration_ms: Some(1000),
            key: format!("{agent}:{n}:{ts}:{model}"),
            ..Row::default()
        }
    }

    fn merged(n: u64, builder: &str, ready: f64, closed: f64) -> Outcome {
        Outcome {
            url: format!("u/{n}"),
            number: n,
            tickets: vec!["SRE-1".into()],
            builder: builder.into(),
            state: "MERGED".into(),
            opened: Some(ready),
            ready: Some(ready),
            closed: Some(closed),
            rounds: Some(n),
            lines: Some(50),
            whole: true,
            ..Outcome::default()
        }
    }

    #[test]
    fn rework_counts_against_the_model_that_built_it() {
        let (e, dir) = engine("rework");
        let mut o = merged(1, "b", 3600.0, 7200.0);
        o.rework = Some(4000.0);
        e.store.save_outcome(&o).unwrap();
        e.store
            .add_spend(&[
                run("b", 1, 3000.0, "sonnet", Some(2.0)),
                run("b-review", 1, 3100.0, "sol", Some(1.0)),
                // The fix: its push is only seen after it ran.
                run("b", 2, 4000.5, "sonnet", Some(1.0)),
                run("b", 3, 5000.0, "opus", Some(5.0)),
                // After the merge, on other tickets, and by others: not its.
                run("b", 4, 9000.0, "sonnet", Some(50.0)),
                Row {
                    tickets: "SRE-9".into(),
                    ..run("b", 5, 3001.0, "sonnet", Some(50.0))
                },
                run("c", 1, 3000.0, "sonnet", Some(50.0)),
            ])
            .unwrap();
        let m = e.models(0.0, 10_000.0).unwrap();
        assert_eq!(
            m.rows,
            [ModelRow {
                size: "small".into(),
                model: "sonnet".into(),
                reviewed: true,
                prs: 1,
                enough: false,
                first: 3.0,
                rework: 6.0,
                unpriced: 0,
                reworked: 1,
                hours: Some(1.0),
                rounds: Some(1.0),
            }]
        );
        assert_eq!(
            (m.merged, m.whole, m.reworked, m.hours),
            (1, 1, 1, Some(1.0))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_builders_open_prs_share_its_runs() {
        let (e, dir) = engine("share");
        e.store
            .save_outcome(&merged(1, "b", 100.0, 1000.0))
            .unwrap();
        e.store
            .save_outcome(&merged(2, "b", 100.0, 2000.0))
            .unwrap();
        e.store
            .add_spend(&[
                run("b", 1, 500.0, "m", Some(4.0)),
                run("b", 2, 1500.0, "m", Some(3.0)),
                run("b", 3, 1600.0, "m", None),
            ])
            .unwrap();
        let m = e.models(0.0, 10_000.0).unwrap();
        assert_eq!(m.rows.len(), 1);
        let r = &m.rows[0];
        assert_eq!((r.prs, r.first, r.rework, r.unpriced), (2, 3.5, 0.0, 1));
        assert_eq!((r.reviewed, r.rounds), (false, Some(1.5)));
        // Only the first merged in this range; its cost doesn't change.
        let m = e.models(0.0, 1500.0).unwrap();
        assert_eq!((m.rows[0].prs, m.rows[0].first), (1, 2.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_send_back_is_rework() {
        let (e, dir) = engine("sent-back");
        e.store
            .save_outcome(&merged(1, "b", 100.0, 1000.0))
            .unwrap();
        e.store
            .add_spend(&[
                run("b", 1, 50.0, "m", Some(1.0)),
                run("b", 2, 300.0, "m", Some(2.0)),
            ])
            .unwrap();
        e.store.add_send_back(200.0, "b").unwrap();
        e.store.add_send_back(5000.0, "b").unwrap();
        e.store.add_send_back(10.0, "someone").unwrap();
        let r = &e.models(0.0, 10_000.0).unwrap().rows[0];
        assert_eq!((r.first, r.rework, r.reworked), (1.0, 2.0, 1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_send_back_before_a_pr_was_opened_is_not_its_rework() {
        let (e, dir) = engine("sent-back-earlier");
        e.store
            .save_outcome(&merged(1, "b", 100.0, 1000.0))
            .unwrap();
        e.store
            .save_outcome(&merged(2, "b", 2000.0, 3000.0))
            .unwrap();
        e.store
            .add_spend(&[
                run("b", 1, 300.0, "m", Some(1.0)),
                run("b", 2, 2500.0, "m", Some(1.0)),
            ])
            .unwrap();
        e.store.add_send_back(200.0, "b").unwrap();
        let m = e.models(0.0, 10_000.0).unwrap();
        assert_eq!(m.reworked, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opened_is_when_github_says_or_the_record_does() {
        let (e, dir) = engine("opened");
        let now = crate::store::now();
        e.observe_pr(None, &pr(now - 60.0), false);
        assert_eq!(e.store.outcomes().unwrap()[0].opened, Some(now - 60.0));
        let mut o = Outcome::default();
        o.apply("pr_linked", &json!({ "state": "OPEN", "draft": true }), 7.0);
        assert_eq!((o.opened, o.ready), (Some(7.0), None));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn says_which_prs_it_leaves_out() {
        let (e, dir) = engine("left-out");
        let on = |n: u64, builder: &str, ticket: &str, model: &str, edit: fn(&mut Outcome)| {
            let mut o = merged(n, builder, 100.0, 1000.0);
            o.tickets = vec![ticket.into()];
            edit(&mut o);
            e.store.save_outcome(&o).unwrap();
            for b in ["x", "y"] {
                if model.is_empty() || (b == "y" && !model.contains('|')) {
                    continue;
                }
                let m = model.split('|').nth(usize::from(b == "y")).unwrap();
                let r = run(
                    &format!("{}{ticket}", if builder.is_empty() { b } else { builder }),
                    i64::from(b == "y"),
                    500.0,
                    m,
                    Some(1.0),
                );
                e.store
                    .add_spend(&[Row {
                        tickets: ticket.into(),
                        key: format!("{n}{b}"),
                        ..r
                    }])
                    .unwrap();
            }
        };
        on(1, "", "SRE-1", "m", |_| {}); // its ticket's only builder
        on(2, "", "SRE-2", "m|m", |_| {}); // two builders
        on(3, "", "SRE-3", "auto (routed)", |_| {});
        on(4, "", "SRE-4", "claude-opus (version unknown)", |_| {});
        on(5, "", "SRE-5", "m", |o| o.whole = false);
        on(6, "gone", "SRE-6", "", |_| {});
        on(7, "", "SRE-7", "m", |o| o.state = "CLOSED".into());
        let m = e.models(0.0, 10_000.0).unwrap();
        assert_eq!((m.merged, m.whole), (6, 5));
        assert_eq!(m.rows.iter().map(|r| r.prs).sum::<usize>(), 1);
        let want = [
            ("no one builder is certain", 1),
            ("not seen from the start", 1),
            ("no runs logged", 1),
            ("several models, or an unknown one", 2),
        ];
        assert_eq!(m.left_out, want.map(|(k, n)| (k.to_string(), n)).into());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_build_split_between_models_is_left_out() {
        for (runs, kept) in [
            (["a", "a", "a", "a", "b"], true),
            (["a", "a", "a", "b", "b"], false),
        ] {
            let (e, dir) = engine(&format!("split-{kept}"));
            e.store
                .save_outcome(&merged(1, "b", 100.0, 1000.0))
                .unwrap();
            for (i, m) in runs.iter().enumerate() {
                e.store
                    .add_spend(&[run("b", i as i64, 200.0 + i as f64, m, Some(1.0))])
                    .unwrap();
            }
            let m = e.models(0.0, 10_000.0).unwrap();
            assert_eq!(m.rows.len() == 1, kept);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn compares_like_with_like() {
        let (e, dir) = engine("like");
        let mut n = 0;
        let mut add = |builder: &str, lines: Option<u64>, harness: &str, reviewed: bool| {
            n += 1;
            let mut o = merged(n, &format!("{builder}{n}"), 100.0, 1000.0);
            o.lines = lines;
            e.store.save_outcome(&o).unwrap();
            let mut rows = vec![Row {
                harness: harness.into(),
                ..run(&o.builder, 1, 500.0, "m", Some(1.0))
            }];
            if reviewed {
                rows.push(run(&reviewer_name(&o.builder), 1, 501.0, "sol", Some(1.0)));
            }
            e.store.add_spend(&rows).unwrap();
        };
        for _ in 0..ENOUGH {
            add("a", Some(100), "claude", true);
        }
        add("b", Some(100), "claude", false);
        add("c", Some(101), "opencode", true);
        add("d", Some(501), "claude", true);
        add("e", None, "claude", true);
        let got: Vec<_> = e
            .models(0.0, 10_000.0)
            .unwrap()
            .rows
            .into_iter()
            .map(|r| (r.size, r.model, r.reviewed, r.prs, r.enough))
            .collect();
        let want = [
            ("small", "m", true, ENOUGH, true),
            ("small", "m", false, 1, false),
            ("medium", "m (OC)", true, 1, false),
            ("large", "m", true, 1, false),
            ("unknown", "m", true, 1, false),
        ];
        assert_eq!(
            got,
            want.map(|(s, m, r, p, e)| (s.to_string(), m.to_string(), r, p, e))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
