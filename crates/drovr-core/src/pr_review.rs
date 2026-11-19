//! PRs that need your review: links posted in Slack channels and GitHub
//! review requests to your teams, offered in the Inbox once each.
//!
//! A poll reads the configured Slack channels (through the Slack provider)
//! and `gh search prs` for each team, looks each new PR up with `gh pr view`,
//! and offers the open, non-draft ones that aren't yours. Each PR is offered
//! once ever, keyed by its URL, in `review.json` in drovr's data folder.

use crate::engine::Engine;
use crate::ledger::Lock;
use crate::shell::{self, quote};
use crate::sources::Sources;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a seen PR is remembered.
const KEEP_SECS: f64 = 90.0 * 86_400.0;

/// Polls a link may fail to look up before drovr stops trying it.
const GIVE_UP_AFTER: u32 = 3;

/// A PR worth offering, and where it came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub url: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    /// `#channel`, or `team org/slug`.
    pub source: String,
    pub permalink: String,
    pub poster: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Slack is read for messages since this (unix seconds).
    since: f64,
    last_poll: f64,
    /// URL to when it was offered.
    seen: BTreeMap<String, f64>,
    /// URL to how many polls in a row couldn't look it up. At
    /// `GIVE_UP_AFTER` it's treated as seen.
    failed: BTreeMap<String, u32>,
}

impl Default for State {
    fn default() -> Self {
        State {
            since: crate::store::now() - 86_400.0,
            last_poll: 0.0,
            seen: BTreeMap::new(),
            failed: BTreeMap::new(),
        }
    }
}

fn path() -> PathBuf {
    crate::config::home().join("review.json")
}

fn load_from(p: &Path) -> State {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(state: &State, p: &Path) -> Result<()> {
    let mut state = state.clone();
    let cutoff = crate::store::now() - KEEP_SECS;
    state.seen.retain(|_, at| *at >= cutoff);
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&state)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// The PR links in a message, normalized to `https://github.com/o/r/pull/n`
/// and deduped in order.
pub(crate) fn pr_urls(text: &str) -> Vec<String> {
    const HOST: &str = "https://github.com/";
    let mut out: Vec<String> = vec![];
    let mut rest = text;
    while let Some(i) = rest.find(HOST) {
        let after = &rest[i + HOST.len()..];
        let end = after
            .find(|c: char| c.is_whitespace() || "<>|()[]\"'`".contains(c))
            .unwrap_or(after.len());
        let link = &after[..end];
        rest = &after[end..];
        let path = link.split(['?', '#']).next().unwrap_or("");
        let mut seg = path.split('/');
        let (Some(owner), Some(repo), Some("pull"), Some(n)) =
            (seg.next(), seg.next(), seg.next(), seg.next())
        else {
            continue;
        };
        if owner.is_empty()
            || repo.is_empty()
            || n.is_empty()
            || !n.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let url = format!("{HOST}{owner}/{repo}/pull/{n}");
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// A PR url's owner, repo and number; only `/pull/<n>` links, with
/// whatever follows the number ignored.
pub(crate) fn parse(url: &str) -> Option<(String, String, u64)> {
    let mut seg = url.strip_prefix("https://github.com/")?.split('/');
    let (owner, repo, kind, n) = (seg.next()?, seg.next()?, seg.next()?, seg.next()?);
    if kind != "pull" || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.into(), repo.into(), n.parse().ok()?))
}

/// A PR's link as drovr keeps it, without anything after the number.
pub(crate) fn canonical(url: &str) -> Option<String> {
    let (owner, repo, n) = parse(url)?;
    Some(format!("https://github.com/{owner}/{repo}/pull/{n}"))
}

/// Look for PRs if a poll is due (always, with `force`), offer the new ones
/// and return them. One poll runs at a time across processes: a forced poll
/// (`drovr review poll`) waits for the one running, the app's timer skips.
pub fn poll(engine: &Engine, force: bool) -> Vec<Found> {
    poll_at(engine, force, &path())
}

/// A PR seen in a source, before it's looked up.
struct Candidate {
    url: String,
    title: String,
    source: String,
    permalink: String,
    poster: String,
}

fn poll_at(engine: &Engine, force: bool, file: &Path) -> Vec<Found> {
    let cfg = &engine.cfg.review;
    if cfg.poll_secs == 0 {
        return vec![];
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file.with_extension("lock"));
    let _one = match &lock_file {
        Ok(f) if force => Some(Lock::exclusive(f)),
        Ok(f) => match Lock::try_exclusive(f) {
            Some(l) => Some(l),
            None => return vec![],
        },
        Err(e) => {
            eprintln!("drovr: couldn't lock PR review polling: {e}");
            None
        }
    };
    let mut state = load_from(file);
    let now = crate::store::now();
    if !force && now - state.last_poll < cfg.poll_secs as f64 {
        return vec![];
    }

    let mut candidates: Vec<Candidate> = vec![];
    let mut slack_ok = true;
    if !cfg.slack_channels.is_empty() {
        match Sources::new(&engine.cfg)
            .slack_pr_messages(&cfg.slack_channels, state.since.max(now - 7.0 * 86_400.0))
        {
            Ok(messages) => {
                for m in messages {
                    let channel = m.channel.trim_start_matches('#');
                    let source = if channel.is_empty() {
                        "Slack".to_string()
                    } else {
                        format!("#{channel}")
                    };
                    for url in pr_urls(&m.text) {
                        candidates.push(Candidate {
                            url,
                            title: String::new(),
                            source: source.clone(),
                            permalink: m.permalink.clone(),
                            poster: m.user.clone(),
                        });
                    }
                }
            }
            Err(e) => {
                slack_ok = false;
                eprintln!("drovr: couldn't read Slack for PRs to review: {e:#}");
            }
        }
    }
    let mut me: Option<String> = None;
    for team in &cfg.github_teams {
        match team_prs(&engine.cfg.watch.gh, team) {
            Ok(prs) => {
                for (url, title, draft, author) in prs {
                    // The search already says which are drafts and whose.
                    let mine =
                        !author.is_empty() && author.eq_ignore_ascii_case(whoami(engine, &mut me));
                    if draft || mine {
                        continue;
                    }
                    candidates.push(Candidate {
                        url,
                        title,
                        source: format!("team {team}"),
                        permalink: String::new(),
                        poster: String::new(),
                    });
                }
            }
            Err(e) => eprintln!("drovr: couldn't search {team}'s review requests: {e:#}"),
        }
    }

    let mut all_ok = slack_ok;
    let mut views: BTreeMap<String, Option<Value>> = BTreeMap::new();
    let mut found: Vec<Found> = vec![];
    let mut offered: Vec<Found> = vec![];
    for c in candidates {
        // Case doesn't matter in a URL; the first source to name a PR keeps it.
        let key = c.url.to_lowercase();
        if state.seen.contains_key(&key) || found.iter().any(|f| f.url.to_lowercase() == key) {
            continue;
        }
        // Other owners' PRs aren't looked up at all.
        if parse(&c.url).is_some_and(|(owner, _, _)| !cfg.allows(&owner)) {
            continue;
        }
        if !views.contains_key(&key) {
            let v = pr_view(&engine.cfg.watch.gh, &c.url)
                .map_err(|e| eprintln!("drovr: couldn't look up {}: {e:#}", c.url))
                .ok();
            if v.is_some() {
                state.failed.remove(&key);
            } else {
                let n = state.failed.entry(key.clone()).or_insert(0);
                *n += 1;
                if *n >= GIVE_UP_AFTER {
                    eprintln!("drovr: giving up on {}", c.url);
                    state.failed.remove(&key);
                    state.seen.insert(key.clone(), now);
                }
            }
            views.insert(key.clone(), v);
        }
        let Some(pr) = views[&key].clone() else {
            if !state.seen.contains_key(&key) {
                all_ok = false;
            }
            continue;
        };
        if pr["state"].as_str() != Some("OPEN") || pr["isDraft"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(canonical) = pr["url"]
            .as_str()
            .and_then(canonical)
            .or_else(|| canonical(&c.url))
        else {
            continue;
        };
        let Some((owner, repo, number)) = parse(&canonical) else {
            continue;
        };
        if !cfg.allows(&owner) {
            continue;
        }
        if state.seen.contains_key(&canonical.to_lowercase())
            || found.iter().any(|f| f.url.eq_ignore_ascii_case(&canonical))
        {
            continue;
        }
        let author = pr["author"]["login"].as_str().unwrap_or("").to_string();
        let me = whoami(engine, &mut me);
        if !me.is_empty() && author.eq_ignore_ascii_case(me) {
            continue;
        }
        let title = pr["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or(&c.title);
        found.push(Found {
            url: canonical,
            owner,
            repo,
            number,
            title: title.to_string(),
            author,
            source: c.source,
            permalink: c.permalink,
            poster: c.poster,
        });
    }

    for f in found {
        if offer(engine, &f) {
            state.seen.insert(f.url.to_lowercase(), now);
            offered.push(f);
        } else {
            all_ok = false;
        }
    }
    let found = offered;
    state.last_poll = now;
    // A failed Slack read, lookup or offer is tried again from the same point.
    if all_ok {
        state.since = now;
    }
    if let Err(e) = save(&state, file) {
        eprintln!("drovr: couldn't save the PR review state: {e:#}");
    }
    if cfg.remove_when_closed {
        remove_closed(engine);
    }
    found
}

/// Remove each reviewer that isn't working whose PR has merged or closed.
fn remove_closed(engine: &Engine) {
    let agents = engine.store.agents(None).unwrap_or_default();
    for a in agents.iter().filter(|a| {
        a.role == "pr-reviewer"
            && matches!(a.run_state.as_str(), "idle" | "done" | "error" | "stopped")
            && !a.ticket.is_empty()
    }) {
        let cmd = format!(
            "{} pr view {} --json state",
            quote(&engine.cfg.watch.gh),
            quote(&a.ticket)
        );
        let state = match gh_json(&cmd) {
            Ok(v) => v["state"].as_str().unwrap_or("").to_uppercase(),
            Err(e) => {
                eprintln!("drovr: couldn't check {}: {e:#}", a.ticket);
                continue;
            }
        };
        if state == "MERGED" || state == "CLOSED" {
            if let Err(e) = engine.remove(&a.name) {
                eprintln!("drovr: couldn't remove reviewer {}: {e:#}", a.name);
            }
        }
    }
}

/// Put a found PR in the Inbox. True if it was saved.
fn offer(engine: &Engine, f: &Found) -> bool {
    let mut body = format!("{}\n\nAuthor: {}\nSource: {}", f.url, f.author, f.source);
    if !f.permalink.is_empty() {
        body += &format!(" ({})", f.permalink);
    }
    if !f.poster.is_empty() {
        body += &format!("\nPosted by: {}", f.poster);
    }
    let payload = json!({
        "url": f.url, "owner": f.owner, "repo": f.repo, "number": f.number,
        "title": f.title, "author": f.author, "source": f.source,
    });
    let title = format!("Review {}/{}#{}: {}", f.owner, f.repo, f.number, f.title);
    let options = ["Dispatch reviewer".to_string(), "Dismiss".to_string()];
    let mut started = None;
    if engine.cfg.review.auto_dispatch {
        match engine.dispatch_review(&f.url) {
            Ok(agent) => started = Some(agent.name),
            Err(e) => {
                eprintln!("drovr: couldn't start a reviewer for {}: {e:#}", f.url);
                body += &format!("\n\nCouldn't start a reviewer: {e:#}");
            }
        }
    }
    match engine
        .store
        .add_item("drovr", "review", &title, &body, &options, &payload)
    {
        Ok(id) => {
            if let Some(name) = started {
                let _ =
                    engine
                        .store
                        .resolve_item(id, "answered", &format!("Reviewer {name} started"));
            }
            engine.record_item(id);
            true
        }
        Err(e) => {
            eprintln!("drovr: couldn't offer {}: {e:#}", f.url);
            // A reviewer already started must not start again next poll.
            started.is_some()
        }
    }
}

fn gh_json(cmd: &str) -> Result<Value> {
    let out = shell::sh(cmd, None, None, Duration::from_secs(60))?.into_stdout(cmd)?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("`{cmd}` didn't print JSON: {e}"))
}

/// The open PRs requesting a team's review, as (url, title, draft, author).
fn team_prs(gh: &str, team: &str) -> Result<Vec<(String, String, bool, String)>> {
    let cmd = format!(
        "{} search prs --review-requested={} --state=open --json url,title,isDraft,author --limit 100",
        quote(gh),
        quote(team)
    );
    Ok(gh_json(&cmd)?
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                p["url"].as_str()?.to_string(),
                p["title"].as_str().unwrap_or("").to_string(),
                p["isDraft"].as_bool().unwrap_or(false),
                p["author"]["login"].as_str().unwrap_or("").to_string(),
            ))
        })
        .collect())
}

/// Your GitHub login, asked once; empty if it can't be.
fn whoami<'a>(engine: &Engine, slot: &'a mut Option<String>) -> &'a str {
    slot.get_or_insert_with(|| {
        gh_user(&engine.cfg.watch.gh).unwrap_or_else(|e| {
            eprintln!("drovr: couldn't tell who you are on GitHub: {e:#}");
            String::new()
        })
    })
}

fn pr_view(gh: &str, url: &str) -> Result<Value> {
    gh_json(&format!(
        "{} pr view {} --json url,title,state,isDraft,author",
        quote(gh),
        quote(url)
    ))
}

fn gh_user(gh: &str) -> Result<String> {
    let cmd = format!("{} api user --jq .login", quote(gh));
    let out = shell::sh(&cmd, None, None, Duration::from_secs(60))?.into_stdout(&cmd)?;
    Ok(out.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;
    use std::path::PathBuf;

    #[test]
    fn extracts_pr_urls() {
        let u = |s: &str| pr_urls(s);
        assert_eq!(
            u("look https://github.com/a/b/pull/3 pls"),
            ["https://github.com/a/b/pull/3"]
        );
        assert_eq!(
            u("<https://github.com/a/b/pull/3|a/b#3>"),
            ["https://github.com/a/b/pull/3"]
        );
        assert_eq!(
            u("https://github.com/a/b/pull/12/files"),
            ["https://github.com/a/b/pull/12"]
        );
        assert_eq!(
            u("https://github.com/a/b/pull/12?x=1"),
            ["https://github.com/a/b/pull/12"]
        );
        assert_eq!(
            u("(https://github.com/a/b/pull/12)."),
            ["https://github.com/a/b/pull/12"]
        );
        assert_eq!(
            u("https://github.com/a/b/pull/1 and <https://github.com/c/d/pull/2|x> again https://github.com/a/b/pull/1/commits"),
            ["https://github.com/a/b/pull/1", "https://github.com/c/d/pull/2"]
        );
        assert!(u(
            "https://github.com/a/b/issues/4 https://github.com/a/b https://github.com/a/b/pull/x"
        )
        .is_empty());
    }

    struct Env {
        dir: PathBuf,
        engine: Engine,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn executable(dir: &Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        write(dir, name, body);
        std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn view(url: &str, state: &str, draft: bool, author: &str) -> String {
        format!(
            r#"{{"url":"{url}","title":"T {url}","state":"{state}","isDraft":{draft},"author":{{"login":"{author}"}}}}"#
        )
    }

    /// A fake `gh` answering from files, and logging every call; Slack is the
    /// `command` provider printing `slack.json`.
    fn env(name: &str, tweak: impl FnOnce(&mut Config)) -> Env {
        let dir =
            std::env::temp_dir().join(format!("drovr-pr-review-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.display();
        write(&dir, "search.json", "[]");
        write(&dir, "slack.json", "[]");
        executable(
            &dir,
            "gh",
            &format!(
                "#!/bin/sh\necho \"$@\" >> {d}/gh.log\ncase \"$1\" in\n search) cat {d}/search.json;;\n api) echo me;;\n pr) cat {d}/view-$(basename \"$3\").json;;\n repo) git clone -q \"$(cat {d}/origin)\" \"$4\" && git -C \"$4\" remote set-url origin \"https://github.com/$3.git\" && git -C \"$4\" config \"url.$(cat {d}/origin).insteadOf\" \"https://github.com/$3.git\";;\nesac\n"
            ),
        );
        let mut cfg = Config::default();
        cfg.watch.gh = dir.join("gh").display().to_string();
        cfg.slack.provider = "command".into();
        cfg.slack.search_cmd = format!("cat {d}/slack.json #{{query}}");
        cfg.review.slack_channels = vec!["#sre-reviews".into()];
        cfg.review.github_teams = vec!["acme/sre".into()];
        tweak(&mut cfg);
        let engine = Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into());
        Env { dir, engine }
    }

    fn items(e: &Env) -> Vec<crate::store::Item> {
        e.engine.store.items(None, 100).unwrap()
    }

    fn team_pr(e: &Env, n: u32, state: &str, draft: bool, author: &str) {
        let url = format!("https://github.com/acme/api/pull/{n}");
        write(
            &e.dir,
            &format!("view-{n}.json"),
            &view(&url, state, draft, author),
        );
    }

    fn search_has(e: &Env, ns: &[u32]) {
        let list: Vec<String> = ns
            .iter()
            .map(|n| format!(r#"{{"url":"https://github.com/acme/api/pull/{n}","title":"T","isDraft":false,"author":{{"login":"x"}}}}"#))
            .collect();
        write(&e.dir, "search.json", &format!("[{}]", list.join(",")));
    }

    #[test]
    fn offers_a_team_pr_once() {
        let e = env("team", |_| {});
        let file = e.dir.join("review.json");
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        let found = poll_at(&e.engine, true, &file);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, "team acme/sre");
        let it = items(&e);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].kind, "review");
        assert_eq!(it[0].agent, "drovr");
        assert_eq!(
            it[0].title,
            "Review acme/api#5: T https://github.com/acme/api/pull/5"
        );
        assert_eq!(it[0].options, ["Dispatch reviewer", "Dismiss"]);
        assert_eq!(
            it[0].payload,
            serde_json::json!({
                "url": "https://github.com/acme/api/pull/5", "owner": "acme", "repo": "api", "number": 5,
                "title": "T https://github.com/acme/api/pull/5", "author": "alice", "source": "team acme/sre"
            })
        );
        assert!(it[0].body.contains("https://github.com/acme/api/pull/5"));
        assert!(it[0].body.contains("alice"));
        assert!(it[0].body.contains("team acme/sre"));

        assert!(poll_at(&e.engine, true, &file).is_empty());
        assert_eq!(items(&e).len(), 1);
    }

    #[test]
    fn the_same_pr_from_slack_and_a_team_is_one_item() {
        let e = env("dupe", |_| {});
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"sre-reviews","user":"bob","ts":"1","text":"pls <https://github.com/acme/api/pull/5|x>","permalink":"https://slack/p1"}]"#,
        );
        let found = poll_at(&e.engine, true, &e.dir.join("review.json"));
        assert_eq!(found.len(), 1);
        assert_eq!(items(&e).len(), 1);
        // Slack is read first, so its source and poster win.
        assert_eq!(found[0].source, "#sre-reviews");
        assert_eq!(found[0].poster, "bob");
        assert_eq!(found[0].permalink, "https://slack/p1");
        let body = &items(&e)[0].body;
        assert!(body.contains("https://slack/p1") && body.contains("bob"));
    }

    #[test]
    fn skips_drafts_closed_and_your_own() {
        let e = env("skip", |_| {});
        team_pr(&e, 1, "OPEN", true, "alice");
        team_pr(&e, 2, "CLOSED", false, "alice");
        team_pr(&e, 3, "OPEN", false, "me");
        team_pr(&e, 4, "MERGED", false, "alice");
        search_has(&e, &[1, 2, 3, 4]);
        assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
        assert!(items(&e).is_empty());
    }

    #[test]
    fn slack_failing_still_offers_the_teams_pr() {
        let e = env("slackfail", |c| {
            c.slack.search_cmd = "exit 1 #{query}".into()
        });
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        let file = e.dir.join("review.json");
        assert_eq!(poll_at(&e.engine, true, &file).len(), 1);
        // The failed Slack read isn't skipped over next time.
        let state = std::fs::read_to_string(&file).unwrap();
        let v: serde_json::Value = serde_json::from_str(&state).unwrap();
        assert!(v["since"].as_f64().unwrap() < crate::store::now() - 80_000.0);
    }

    #[test]
    fn a_failing_gh_search_still_offers_slacks_pr() {
        let e = env("ghfail", |_| {});
        executable(&e.dir, "gh", "#!/bin/sh\ncase \"$1\" in search) exit 1;; api) echo me;; pr) cat \"$(dirname \"$0\")/view-$(basename \"$3\").json\";; esac\n");
        team_pr(&e, 5, "OPEN", false, "alice");
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"sre-reviews","user":"bob","ts":"1","text":"https://github.com/acme/api/pull/5","permalink":""}]"#,
        );
        assert_eq!(
            poll_at(&e.engine, true, &e.dir.join("review.json")).len(),
            1
        );
    }

    #[test]
    fn does_nothing_when_off() {
        for (name, tweak) in [
            (
                "off0",
                Box::new(|c: &mut Config| c.review.poll_secs = 0) as Box<dyn FnOnce(&mut Config)>,
            ),
            (
                "offnone",
                Box::new(|c: &mut Config| {
                    c.review.slack_channels.clear();
                    c.review.github_teams.clear();
                }),
            ),
        ] {
            let e = env(name, tweak);
            team_pr(&e, 5, "OPEN", false, "alice");
            search_has(&e, &[5]);
            assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
            assert!(!e.dir.join("gh.log").exists(), "{name} called gh");
            assert!(items(&e).is_empty());
        }
    }

    #[test]
    fn waits_until_a_poll_is_due() {
        let e = env("due", |_| {});
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        let file = e.dir.join("review.json");
        let now = crate::store::now();
        write(
            &e.dir,
            "review.json",
            &format!(
                r#"{{"since":{},"last_poll":{},"seen":{{}}}}"#,
                now - 30.0,
                now - 30.0
            ),
        );
        assert!(poll_at(&e.engine, false, &file).is_empty());
        assert!(!e.dir.join("gh.log").exists());
        assert_eq!(poll_at(&e.engine, true, &file).len(), 1);

        // And once it's been long enough, it polls without being forced.
        let e = env("due2", |_| {});
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        let file = e.dir.join("review.json");
        write(
            &e.dir,
            "review.json",
            &format!(
                r#"{{"since":{},"last_poll":{},"seen":{{}}}}"#,
                now - 700.0,
                now - 700.0
            ),
        );
        assert_eq!(poll_at(&e.engine, false, &file).len(), 1);
    }

    #[test]
    fn dedupes_urls_case_insensitively() {
        let e = env("case", |_| {});
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"sre-reviews","user":"bob","ts":"1","text":"https://github.com/Acme/API/pull/5","permalink":""}]"#,
        );
        let file = e.dir.join("review.json");
        let found = poll_at(&e.engine, true, &file);
        assert_eq!(found.len(), 1);
        assert_eq!(items(&e).len(), 1);
        assert_eq!(found[0].url, "https://github.com/acme/api/pull/5");
        assert_eq!(
            (found[0].owner.as_str(), found[0].repo.as_str()),
            ("acme", "api")
        );
        assert!(poll_at(&e.engine, true, &file).is_empty());
    }

    #[test]
    fn slack_lookback_is_bounded() {
        let e = env("lookback", |c| {
            let d = std::env::temp_dir()
                .join(format!("drovr-pr-review-lookback-{}", std::process::id()));
            c.slack.search_cmd = format!(
                "echo {{query}} >> {d}/slack.log; cat {d}/slack.json",
                d = d.display()
            );
        });
        let now = crate::store::now();
        let file = e.dir.join("review.json");
        write(
            &e.dir,
            "review.json",
            &format!(
                r#"{{"since":{},"last_poll":0,"seen":{{}}}}"#,
                now - 30.0 * 86_400.0
            ),
        );
        poll_at(&e.engine, true, &file);
        let log = std::fs::read_to_string(e.dir.join("slack.log")).unwrap();
        let day = log
            .split("after:")
            .nth(1)
            .unwrap()
            .get(..10)
            .unwrap()
            .to_string();
        let oldest = crate::ledger::iso(now - 8.0 * 86_400.0)[..10].to_string();
        assert!(day >= oldest, "{day} is older than {oldest}");
    }

    #[test]
    fn looks_up_each_pr_once_per_poll() {
        let e = env("cache", |_| {});
        team_pr(&e, 5, "OPEN", true, "alice");
        search_has(&e, &[5]);
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"c","user":"b","ts":"1","text":"https://github.com/acme/api/pull/5 https://github.com/Acme/Api/pull/5","permalink":""}]"#,
        );
        assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
        let log = std::fs::read_to_string(e.dir.join("gh.log")).unwrap();
        assert_eq!(log.lines().filter(|l| l.starts_with("pr view")).count(), 1);
        let state = std::fs::read_to_string(e.dir.join("review.json")).unwrap();
        assert!(!state.contains("pull/5"), "a draft must not be marked seen");
    }

    #[test]
    fn state_drops_old_entries_and_survives_corruption() {
        let dir =
            std::env::temp_dir().join(format!("drovr-pr-review-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("review.json");
        let now = crate::store::now();
        let mut s = State::default();
        s.seen.insert("old".into(), now - 91.0 * 86_400.0);
        s.seen.insert("new".into(), now - 89.0 * 86_400.0);
        save(&s, &file).unwrap();
        let back = load_from(&file);
        assert_eq!(back.seen.keys().collect::<Vec<_>>(), ["new"]);

        std::fs::write(&file, "{not json").unwrap();
        let fresh = load_from(&file);
        assert!(fresh.seen.is_empty());
        assert!((fresh.since - (now - 86_400.0)).abs() < 5.0);
        assert_eq!(fresh.last_poll, 0.0);
    }

    fn git(dir: &Path, args: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("git -c user.name=t -c user.email=t@t {args}"))
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare origin whose `refs/pull/7/head` is one commit past `main`.
    /// Returns the PR's commit.
    fn origin(e: &Env) -> String {
        let bare = e.dir.join("origin.git");
        let work = e.dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&e.dir, "init -q --bare -b main origin.git");
        git(&work, "init -q -b main");
        git(&work, "commit -q --allow-empty -m one");
        git(&work, &format!("push -q {} main", bare.display()));
        std::fs::write(work.join("f.txt"), "pr").unwrap();
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::write(work.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(work.join(".mcp.json"), "{}").unwrap();
        git(&work, "add -A");
        git(&work, "commit -q -m two");
        git(
            &work,
            &format!("push -q {} HEAD:refs/pull/7/head", bare.display()),
        );
        write(&e.dir, "origin", &bare.display().to_string());
        git(&work, "rev-parse HEAD")
    }

    fn clone_repo(e: &Env, repo: &str) {
        let repos = e.dir.join("repos");
        std::fs::create_dir_all(&repos).unwrap();
        git(
            &e.dir,
            &format!("clone -q origin.git {}", repos.join(repo).display()),
        );
        let url = format!("https://github.com/acme/{repo}.git");
        let clone = repos.join(repo);
        git(&clone, &format!("remote set-url origin {url}"));
        git(
            &clone,
            &format!(
                "config url.{}.insteadOf {url}",
                e.dir.join("origin.git").display()
            ),
        );
    }

    fn review_env(name: &str, tweak: impl FnOnce(&mut Config)) -> Env {
        let repos = std::env::temp_dir()
            .join(format!("drovr-pr-review-{name}-{}", std::process::id()))
            .join("repos");
        env(name, |c| {
            c.claude.command = "true".into();
            c.defaults.auto_wake = false;
            c.review.repos_dir = repos.display().to_string();
            c.stages.models.insert("review".into(), "rev-model".into());
            c.stages.review_skill = "/s/SKILL.md".into();
            tweak(c);
        })
    }

    fn url(repo: &str, n: u32) -> String {
        format!("https://github.com/acme/{repo}/pull/{n}")
    }

    fn worktree(name: &str) -> PathBuf {
        crate::config::home().join("worktrees").join(name)
    }

    fn reviewer(e: &Env, name: &str, run_state: &str, ticket: &str, cwd: &str) {
        e.engine
            .store
            .add_agent(&crate::store::Agent {
                name: name.into(),
                role: "pr-reviewer".into(),
                run_state: run_state.into(),
                ticket: ticket.into(),
                cwd: cwd.into(),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn dispatch_starts_a_reviewer_at_the_prs_head() {
        let e = review_env("disp", |_| {});
        let sha = origin(&e);
        clone_repo(&e, "rvdisp");
        let a = e.engine.dispatch_review(&url("rvdisp", 7)).unwrap();
        assert_eq!(a.name, "review-rvdisp-7");
        assert_eq!(a.role, "pr-reviewer");
        assert_eq!(a.ticket, url("rvdisp", 7));
        assert_eq!(a.model, "rev-model");
        assert_eq!(a.cwd, worktree("review-rvdisp-7").display().to_string());
        assert_eq!(git(&Path::new(&a.cwd).join("pr"), "rev-parse HEAD"), sha);
        // The PR's own agent config must not load: none of it is at the root.
        for f in [".claude", ".mcp.json", ".opencode", "opencode.json"] {
            assert!(!Path::new(&a.cwd).join(f).exists(), "{f} at the root");
        }
        assert!(Path::new(&a.cwd).join("pr/.claude/settings.json").exists());
        assert!(Path::new(&a.cwd).join("pr/.mcp.json").exists());
        for want in [
            url("rvdisp", 7).as_str(),
            "/s/SKILL.md",
            "read-only",
            "Review acme/rvdisp#7",
            "git -C pr",
        ] {
            assert!(a.task.contains(want), "{want} not in {}", a.task);
        }
        let _ = e.engine.remove(&a.name);
    }

    #[test]
    fn dispatch_clones_a_missing_repo() {
        let e = review_env("clone", |_| {});
        origin(&e);
        assert!(!e.dir.join("repos/rvclone").exists());
        let a = e.engine.dispatch_review(&url("rvclone", 7)).unwrap();
        assert!(e.dir.join("repos/rvclone/.git").exists());
        assert_eq!(a.name, "review-rvclone-7");
        let _ = e.engine.remove(&a.name);
    }

    #[test]
    fn a_failed_fetch_leaves_nothing_behind() {
        let e = review_env("nofetch", |_| {});
        origin(&e);
        clone_repo(&e, "rvnofetch");
        assert!(e.engine.dispatch_review(&url("rvnofetch", 99)).is_err());
        assert!(e
            .engine
            .store
            .agent("review-rvnofetch-99")
            .unwrap()
            .is_none());
        assert!(!worktree("review-rvnofetch-99").exists());
    }

    #[test]
    fn a_second_reviewer_for_the_same_pr_gets_its_own_name() {
        let e = review_env("twice", |_| {});
        origin(&e);
        clone_repo(&e, "rvtwice");
        let a = e.engine.dispatch_review(&url("rvtwice", 7)).unwrap();
        let b = e.engine.dispatch_review(&url("rvtwice", 7)).unwrap();
        assert_eq!(a.name, "review-rvtwice-7");
        assert_eq!(b.name, "review-rvtwice-7-2");
        let _ = e.engine.remove(&a.name);
        let _ = e.engine.remove(&b.name);
    }

    fn offer_pr(e: &Env, repo: &str, n: u64) -> i64 {
        let f = Found {
            url: url(repo, n as u32),
            owner: "acme".into(),
            repo: repo.into(),
            number: n,
            title: "T".into(),
            author: "alice".into(),
            source: "#c".into(),
            ..Default::default()
        };
        assert!(offer(&e.engine, &f));
        items(e).iter().find(|i| i.status == "pending").unwrap().id
    }

    #[test]
    fn answering_dispatch_starts_the_reviewer() {
        let e = review_env("ans", |_| {});
        origin(&e);
        clone_repo(&e, "rvans");
        let id = offer_pr(&e, "rvans", 7);
        e.engine.answer(id, "Dispatch reviewer").unwrap();
        let item = e.engine.store.item(id).unwrap().unwrap();
        assert_eq!(item.status, "answered");
        assert_eq!(item.answer, "Reviewer review-rvans-7 started");
        assert!(e.engine.store.agent("review-rvans-7").unwrap().is_some());
        let _ = e.engine.remove("review-rvans-7");
    }

    #[test]
    fn answering_dismiss_starts_nothing() {
        let e = review_env("dismiss", |_| {});
        origin(&e);
        clone_repo(&e, "rvdismiss");
        let id = offer_pr(&e, "rvdismiss", 7);
        e.engine.answer(id, "Dismiss").unwrap();
        let item = e.engine.store.item(id).unwrap().unwrap();
        assert_eq!(item.status, "dismissed");
        assert!(e.engine.store.agents(None).unwrap().is_empty());
    }

    #[test]
    fn a_failed_dispatch_leaves_the_item_pending() {
        let e = review_env("anserr", |_| {});
        origin(&e);
        clone_repo(&e, "rvanserr");
        let id = offer_pr(&e, "rvanserr", 99);
        assert!(e.engine.answer(id, "Dispatch reviewer").is_err());
        assert_eq!(e.engine.store.item(id).unwrap().unwrap().status, "pending");
        assert!(e.engine.store.agents(None).unwrap().is_empty());
    }

    #[test]
    fn removing_a_reviewer_takes_its_worktree_and_branch() {
        let e = review_env("rm", |_| {});
        origin(&e);
        clone_repo(&e, "rvrm");
        let a = e.engine.dispatch_review(&url("rvrm", 7)).unwrap();
        std::fs::write(Path::new(&a.cwd).join("pr/scratch.txt"), "x").unwrap();
        std::fs::write(Path::new(&a.cwd).join("stray.txt"), "x").unwrap();
        let note = e.engine.remove(&a.name).unwrap();
        assert_eq!(note, "Its worktree and branch were removed.");
        assert!(!Path::new(&a.cwd).exists());
        let clone = e.dir.join("repos/rvrm");
        assert_eq!(git(&clone, "branch --list drovr/review-rvrm-7"), "");
        assert!(!git(&clone, "worktree list").contains("review-rvrm-7"));
    }

    #[test]
    fn removing_a_solo_agent_still_keeps_a_dirty_worktree() {
        let e = review_env("solo", |c| c.defaults.worktrees = true);
        origin(&e);
        clone_repo(&e, "rvsolo");
        let repo = e.dir.join("repos/rvsolo");
        let a = e
            .engine
            .start_agent(crate::engine::NewAgent {
                name: "rvsolo-agent".into(),
                cwd: repo.display().to_string(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.cwd, worktree("rvsolo-agent").display().to_string());
        std::fs::write(Path::new(&a.cwd).join("scratch.txt"), "x").unwrap();
        let note = e.engine.remove(&a.name).unwrap();
        assert!(note.contains("uncommitted changes"), "{note}");
        assert!(Path::new(&a.cwd).exists());
        assert!(
            git(&repo, "branch --list drovr/rvsolo-agent").contains("drovr/rvsolo-agent"),
            "branch kept"
        );
    }

    #[test]
    fn a_role_lookalike_outside_its_worktree_is_not_force_removed() {
        let e = review_env("look", |_| {});
        let elsewhere = e.dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        reviewer(&e, "rvlook-a", "idle", "", &elsewhere.display().to_string());
        assert_eq!(e.engine.remove("rvlook-a").unwrap(), "");
        assert!(elsewhere.exists());
    }

    fn closed_state(name: &str, state: &str, tweak: impl FnOnce(&mut Config)) -> (Env, PathBuf) {
        let e = review_env(name, tweak);
        write(
            &e.dir,
            "view-7.json",
            &view(&url("api", 7), state, false, "x"),
        );
        let file = e.dir.join("review.json");
        (e, file)
    }

    #[test]
    fn an_idle_reviewer_on_a_merged_pr_is_removed() {
        for state in ["MERGED", "CLOSED"] {
            let (e, file) = closed_state(&format!("gone{state}"), state, |_| {});
            // Stored in another case than gh prints it.
            reviewer(&e, "rvgone", "idle", &url("api", 7).to_uppercase(), "/tmp");
            poll_at(&e.engine, true, &file);
            assert!(e.engine.store.agent("rvgone").unwrap().is_none(), "{state}");
        }
    }

    #[test]
    fn working_reviewers_and_open_prs_are_kept() {
        let (e, file) = closed_state("keepw", "MERGED", |_| {});
        reviewer(&e, "rvkeepw", "working", &url("api", 7), "/tmp");
        poll_at(&e.engine, true, &file);
        assert!(e.engine.store.agent("rvkeepw").unwrap().is_some());

        let (e, file) = closed_state("keepo", "OPEN", |_| {});
        reviewer(&e, "rvkeepo", "idle", &url("api", 7), "/tmp");
        poll_at(&e.engine, true, &file);
        assert!(e.engine.store.agent("rvkeepo").unwrap().is_some());

        let (e, file) = closed_state("keepoff", "MERGED", |c| c.review.remove_when_closed = false);
        reviewer(&e, "rvkeepoff", "idle", &url("api", 7), "/tmp");
        poll_at(&e.engine, true, &file);
        assert!(e.engine.store.agent("rvkeepoff").unwrap().is_some());
    }

    #[test]
    fn auto_dispatch_starts_a_reviewer_and_records_it() {
        let e = review_env("auto", |c| c.review.auto_dispatch = true);
        origin(&e);
        clone_repo(&e, "rvauto");
        write(
            &e.dir,
            "view-7.json",
            &view(&url("rvauto", 7), "OPEN", false, "alice"),
        );
        write(
            &e.dir,
            "search.json",
            &format!(
                r#"[{{"url":"{}","title":"T","isDraft":false,"author":{{"login":"alice"}}}}]"#,
                url("rvauto", 7)
            ),
        );
        let found = poll_at(&e.engine, true, &e.dir.join("review.json"));
        assert_eq!(found.len(), 1);
        assert!(e.engine.store.agent("review-rvauto-7").unwrap().is_some());
        let it = items(&e);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].status, "answered");
        assert_eq!(it[0].answer, "Reviewer review-rvauto-7 started");
        let _ = e.engine.remove("review-rvauto-7");
    }

    #[test]
    fn a_failed_auto_dispatch_falls_back_to_a_pending_item() {
        let e = review_env("autofail", |c| c.review.auto_dispatch = true);
        origin(&e);
        clone_repo(&e, "rvautofail");
        write(
            &e.dir,
            "view-99.json",
            &view(&url("rvautofail", 99), "OPEN", false, "alice"),
        );
        write(
            &e.dir,
            "search.json",
            &format!(
                r#"[{{"url":"{}","title":"T","isDraft":false,"author":{{"login":"alice"}}}}]"#,
                url("rvautofail", 99)
            ),
        );
        let found = poll_at(&e.engine, true, &e.dir.join("review.json"));
        assert_eq!(found.len(), 1);
        let it = items(&e);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].status, "pending");
        assert!(it[0].body.contains("fetch"), "{}", it[0].body);
        assert!(e.engine.store.agents(None).unwrap().is_empty());
    }

    #[test]
    fn dispatch_uses_the_remote_that_points_at_the_prs_repo() {
        let e = review_env("remote", |_| {});
        origin(&e);
        clone_repo(&e, "rvremote");
        let clone = e.dir.join("repos/rvremote");
        git(
            &clone,
            "remote set-url origin https://github.com/someone/rvremote.git",
        );
        git(
            &clone,
            "remote add upstream git@github.com:Acme/RVRemote.git",
        );
        git(
            &clone,
            &format!(
                "config url.{}.insteadOf git@github.com:Acme/RVRemote.git",
                e.dir.join("origin.git").display()
            ),
        );
        let a = e.engine.dispatch_review(&url("rvremote", 7)).unwrap();
        assert!(Path::new(&a.cwd).join("pr/f.txt").exists());
        assert_eq!(git(&clone, "for-each-ref refs/drovr"), "");
        let _ = e.engine.remove(&a.name);
    }

    #[test]
    fn dispatch_without_a_matching_remote_errors_cleanly() {
        let e = review_env("noremote", |_| {});
        origin(&e);
        clone_repo(&e, "rvnoremote");
        let clone = e.dir.join("repos/rvnoremote");
        git(
            &clone,
            "remote set-url origin https://github.com/someone/rvnoremote.git",
        );
        let err = e.engine.dispatch_review(&url("rvnoremote", 7)).unwrap_err();
        assert!(
            err.to_string()
                .contains("has no remote for acme/rvnoremote"),
            "{err:#}"
        );
        assert!(e.engine.store.agents(None).unwrap().is_empty());
        assert!(!worktree("review-rvnoremote-7").exists());
    }

    #[test]
    fn dispatch_refuses_a_folder_that_isnt_the_clone() {
        let e = review_env("notgit", |_| {});
        origin(&e);
        std::fs::create_dir_all(e.dir.join("repos/rvnotgit")).unwrap();
        assert!(e.engine.dispatch_review(&url("rvnotgit", 7)).is_err());
        assert!(!worktree("review-rvnotgit-7").exists());
    }

    #[test]
    fn remote_urls_match_in_every_form() {
        for u in [
            "https://github.com/Acme/Api.git",
            "https://github.com/acme/api",
            "git@github.com:acme/api.git",
            "ssh://git@github.com/acme/api",
        ] {
            assert!(crate::engine::remote_is(u, "acme", "api"), "{u}");
        }
        for u in [
            "https://github.com/acme/api-x.git",
            "https://github.com/xacme/api",
            "/srv/acme/api",
        ] {
            assert!(!crate::engine::remote_is(u, "acme", "api"), "{u}");
        }
    }

    #[test]
    fn only_listed_owners_are_offered_or_dispatched() {
        let e = review_env("owners", |c| {
            c.review.owners = vec!["ACME".into()];
        });
        origin(&e);
        clone_repo(&e, "rvowners");
        let a = e.engine.dispatch_review(&url("rvowners", 7)).unwrap();
        let _ = e.engine.remove(&a.name);
        let err = e
            .engine
            .dispatch_review("https://github.com/other/rvowners/pull/7")
            .unwrap_err();
        assert!(err.to_string().contains("review.owners"), "{err:#}");
        assert!(!worktree("review-rvowners-7-2").exists());

        let e = review_env("owners2", |c| c.review.owners = vec!["other".into()]);
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
        assert!(items(&e).is_empty());
    }

    #[test]
    fn dispatch_rejects_odd_owners_and_repos() {
        let e = review_env("odd", |_| {});
        for u in [
            "https://github.com/acme/../pull/7",
            "https://github.com/acme/-x/pull/7",
            "https://github.com/-o/r/pull/7",
            "https://github.com/acme/.hid/pull/7",
        ] {
            assert!(e.engine.dispatch_review(u).is_err(), "{u}");
        }
    }

    #[test]
    fn long_repo_names_keep_the_number() {
        let e = review_env("long", |_| {});
        origin(&e);
        let repo = "rvlong-a-very-long-repository-name-indeed-yes";
        clone_repo(&e, repo);
        let a = e.engine.dispatch_review(&url(repo, 7)).unwrap();
        let b = e.engine.dispatch_review(&url(repo, 7)).unwrap();
        assert!(a.name.len() <= 40 && a.name.ends_with("-7"), "{}", a.name);
        assert!(b.name.len() <= 40 && b.name.ends_with("-7-2"), "{}", b.name);
        let _ = e.engine.remove(&a.name);
        let _ = e.engine.remove(&b.name);
    }

    #[test]
    fn a_reviewer_that_cant_start_leaves_no_worktree_or_branch() {
        let e = review_env("startfail", |c| c.defaults.harness = "nope".into());
        origin(&e);
        clone_repo(&e, "rvstartfail");
        assert!(e.engine.dispatch_review(&url("rvstartfail", 7)).is_err());
        let clone = e.dir.join("repos/rvstartfail");
        assert!(!worktree("review-rvstartfail-7").exists());
        assert_eq!(git(&clone, "worktree list").lines().count(), 1);
        assert_eq!(git(&clone, "branch --list drovr/*"), "");
        assert_eq!(git(&clone, "for-each-ref refs/drovr"), "");
    }

    #[test]
    fn concurrent_dispatches_get_distinct_reviewers() {
        let e = review_env("conc", |_| {});
        origin(&e);
        clone_repo(&e, "rvconc");
        let u = url("rvconc", 7);
        let names: Vec<String> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..3)
                .map(|_| {
                    s.spawn(|| {
                        let engine = Engine::with(
                            Store::open(&e.dir.join("t.db")).unwrap(),
                            e.engine.cfg.clone(),
                            "drovr".into(),
                        );
                        engine.dispatch_review(&u).unwrap().name
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "{names:?}");
        for n in names {
            let _ = e.engine.remove(&n);
        }
    }

    #[test]
    fn a_held_lock_makes_the_timers_poll_skip() {
        let e = env("lock", |_| {});
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        let file = e.dir.join("review.json");
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(e.dir.join("review.lock"))
            .unwrap();
        let _l = Lock::exclusive(&held);
        assert!(poll_at(&e.engine, false, &file).is_empty());
        assert!(!e.dir.join("gh.log").exists());
    }

    #[test]
    fn a_failed_lookup_keeps_slacks_since_and_is_retried() {
        let e = env("retry", |c| c.review.github_teams.clear());
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"c","user":"b","ts":"1","text":"https://github.com/acme/api/pull/5","permalink":""}]"#,
        );
        let file = e.dir.join("review.json");
        assert!(poll_at(&e.engine, true, &file).is_empty());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v["since"].as_f64().unwrap() < crate::store::now() - 80_000.0);
        team_pr(&e, 5, "OPEN", false, "alice");
        assert_eq!(poll_at(&e.engine, true, &file).len(), 1);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v["since"].as_f64().unwrap() > crate::store::now() - 100.0);
    }

    #[test]
    fn reviewers_are_removed_with_no_sources_configured() {
        let (e, file) = closed_state("nosrc", "MERGED", |c| {
            c.review.slack_channels.clear();
            c.review.github_teams.clear();
        });
        for (n, st) in [
            ("rvnosrc-i", "idle"),
            ("rvnosrc-d", "done"),
            ("rvnosrc-e", "error"),
            ("rvnosrc-s", "stopped"),
        ] {
            reviewer(&e, n, st, &url("api", 7), "/tmp");
        }
        reviewer(&e, "rvnosrc-w", "working", &url("api", 7), "/tmp");
        poll_at(&e.engine, true, &file);
        let left: Vec<String> = e
            .engine
            .store
            .agents(None)
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(left, ["rvnosrc-w"]);
    }

    #[test]
    fn only_pull_links_parse() {
        assert!(parse("https://github.com/a/b/issues/5/x").is_none());
        assert!(parse("https://github.com/a/b/pull/x").is_none());
        assert_eq!(
            canonical("https://github.com/a/b/pull/5/files").unwrap(),
            "https://github.com/a/b/pull/5"
        );
    }

    #[test]
    fn the_search_drops_drafts_and_your_own_before_looking_them_up() {
        let e = env("prefilter", |c| c.review.slack_channels.clear());
        write(
            &e.dir,
            "search.json",
            r#"[{"url":"https://github.com/acme/api/pull/1","title":"T","isDraft":true,"author":{"login":"x"}},{"url":"https://github.com/acme/api/pull/2","title":"T","isDraft":false,"author":{"login":"ME"}}]"#,
        );
        assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
        let log = std::fs::read_to_string(e.dir.join("gh.log")).unwrap();
        assert!(!log.contains("pr view"), "{log}");
    }

    #[test]
    fn a_link_that_never_resolves_is_given_up_on() {
        let e = env("dead", |c| c.review.github_teams.clear());
        write(
            &e.dir,
            "slack.json",
            r#"[{"channel":"c","user":"b","ts":"1","text":"https://github.com/acme/api/pull/5","permalink":""}]"#,
        );
        let file = e.dir.join("review.json");
        let since = |file: &Path| {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
            v["since"].as_f64().unwrap()
        };
        let old = crate::store::now() - 80_000.0;
        for _ in 0..2 {
            assert!(poll_at(&e.engine, true, &file).is_empty());
            assert!(since(&file) < old);
        }
        assert!(poll_at(&e.engine, true, &file).is_empty());
        assert!(since(&file) > old);
        // It's given up on for good, even once gh can answer.
        team_pr(&e, 5, "OPEN", false, "alice");
        assert!(poll_at(&e.engine, true, &file).is_empty());
        assert!(items(&e).is_empty());
    }

    #[test]
    fn other_owners_cost_no_lookup() {
        let e = env("noowner", |c| c.review.owners = vec!["other".into()]);
        team_pr(&e, 5, "OPEN", false, "alice");
        search_has(&e, &[5]);
        assert!(poll_at(&e.engine, true, &e.dir.join("review.json")).is_empty());
        let log = std::fs::read_to_string(e.dir.join("gh.log")).unwrap();
        assert!(!log.contains("pr view"), "{log}");
    }
}
