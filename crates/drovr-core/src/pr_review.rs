//! PRs that need your review: links posted in Slack channels and GitHub
//! review requests to your teams, offered in the Inbox once each.
//!
//! A poll reads the configured Slack channels (through the Slack provider)
//! and `gh search prs` for each team, looks each new PR up with `gh pr view`,
//! and offers the open, non-draft ones that aren't yours. Each PR is offered
//! once ever, keyed by its URL, in `review.json` in drovr's data folder.

use crate::engine::Engine;
use crate::shell::{self, quote};
use crate::sources::Sources;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// How long a seen PR is remembered.
const KEEP_SECS: f64 = 90.0 * 86_400.0;

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
}

impl Default for State {
    fn default() -> Self {
        State {
            since: crate::store::now() - 86_400.0,
            last_poll: 0.0,
            seen: BTreeMap::new(),
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
pub fn pr_urls(text: &str) -> Vec<String> {
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

/// A PR url's owner, repo and number.
fn parse(url: &str) -> Option<(String, String, u64)> {
    let mut seg = url.strip_prefix("https://github.com/")?.split('/');
    let (owner, repo, _, n) = (seg.next()?, seg.next()?, seg.next()?, seg.next()?);
    Some((owner.into(), repo.into(), n.parse().ok()?))
}

/// One poll at a time, whether from the timer or `drovr review poll`.
static POLLING: Mutex<()> = Mutex::new(());

/// Look for PRs if a poll is due (always, with `force`), offer the new ones
/// and return them.
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
    if cfg.poll_secs == 0 || (cfg.slack_channels.is_empty() && cfg.github_teams.is_empty()) {
        return vec![];
    }
    let _one = POLLING.lock().unwrap_or_else(|e| e.into_inner());
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
    for team in &cfg.github_teams {
        match team_prs(&engine.cfg.watch.gh, team) {
            Ok(urls) => candidates.extend(urls.into_iter().map(|(url, title)| Candidate {
                url,
                title,
                source: format!("team {team}"),
                permalink: String::new(),
                poster: String::new(),
            })),
            Err(e) => eprintln!("drovr: couldn't search {team}'s review requests: {e:#}"),
        }
    }

    let mut me: Option<String> = None;
    let mut views: BTreeMap<String, Option<Value>> = BTreeMap::new();
    let mut found: Vec<Found> = vec![];
    let mut offered: Vec<Found> = vec![];
    for c in candidates {
        // Case doesn't matter in a URL; the first source to name a PR keeps it.
        let key = c.url.to_lowercase();
        if state.seen.contains_key(&key) || found.iter().any(|f| f.url.to_lowercase() == key) {
            continue;
        }
        let view = views.entry(key).or_insert_with(|| {
            pr_view(&engine.cfg.watch.gh, &c.url)
                .map_err(|e| eprintln!("drovr: couldn't look up {}: {e:#}", c.url))
                .ok()
        });
        let Some(pr) = view.clone() else {
            continue;
        };
        if pr["state"].as_str() != Some("OPEN") || pr["isDraft"].as_bool().unwrap_or(false) {
            continue;
        }
        let canonical = pr["url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .unwrap_or(&c.url);
        let Some((owner, repo, number)) = parse(canonical).or_else(|| parse(&c.url)) else {
            continue;
        };
        let canonical = canonical.to_string();
        if state.seen.contains_key(&canonical.to_lowercase())
            || found.iter().any(|f| f.url.eq_ignore_ascii_case(&canonical))
        {
            continue;
        }
        let author = pr["author"]["login"].as_str().unwrap_or("").to_string();
        let me = me.get_or_insert_with(|| {
            gh_user(&engine.cfg.watch.gh).unwrap_or_else(|e| {
                eprintln!("drovr: couldn't tell who you are on GitHub: {e:#}");
                String::new()
            })
        });
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
        }
    }
    let found = offered;
    state.last_poll = now;
    // A failed Slack read is tried again from the same point.
    if slack_ok {
        state.since = now;
    }
    if let Err(e) = save(&state, file) {
        eprintln!("drovr: couldn't save the PR review state: {e:#}");
    }
    found
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
    match engine
        .store
        .add_item("drovr", "review", &title, &body, &options, &payload)
    {
        Ok(id) => {
            engine.record_item(id);
            true
        }
        Err(e) => {
            eprintln!("drovr: couldn't offer {}: {e:#}", f.url);
            false
        }
    }
}

fn gh_json(cmd: &str) -> Result<Value> {
    let out = shell::sh(cmd, None, None, Duration::from_secs(60))?.into_stdout(cmd)?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("`{cmd}` didn't print JSON: {e}"))
}

/// The open PRs requesting a team's review, as (url, title).
fn team_prs(gh: &str, team: &str) -> Result<Vec<(String, String)>> {
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
            ))
        })
        .collect())
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
                "#!/bin/sh\necho \"$@\" >> {d}/gh.log\ncase \"$1\" in\n search) cat {d}/search.json;;\n api) echo me;;\n pr) cat {d}/view-$(basename \"$3\").json;;\nesac\n"
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
}
