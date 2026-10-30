//! Reviewing an agent's work: the git repos it touched, their changes and
//! diffs, and local HTML pages.
//!
//! An agent often starts in a folder above several repos and makes its own
//! worktrees, so the repos come from what it did: the files it edited, the
//! folders it ran commands in, and the worktrees it added, read from its
//! Claude Code session and drovr's run logs. Each repo's changes are its
//! branch since it left the default branch, plus anything uncommitted.

use crate::config::user_home;
use crate::shell::{self, quote};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Don't list more repos than this for one agent.
const MAX_REPOS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// `M`, `A`, `D`, `R` (against the base), or `??` for untracked.
    pub status: String,
    /// Not committed yet.
    pub uncommitted: bool,
    pub is_html: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    /// The repo's (or worktree's) top-level folder.
    pub root: String,
    /// `root`, shortened: relative to the agent's folder, or with `~`.
    pub label: String,
    /// Branch, commits ahead of the base, and the last commit.
    pub summary: String,
    /// What the changes are measured against, e.g. `origin/main`, or `HEAD`.
    pub base: String,
    pub changes: Vec<Change>,
    pub html: Vec<String>,
    /// In a repo many agents' work shares (the one holding the plans
    /// folder, or `~/.claude`): the folders and files, relative to `root`,
    /// this agent worked in. Its changes and diffs are only those. Empty:
    /// the whole repo.
    pub scope: Vec<String>,
}

fn git(cwd: &str, args: &str) -> Result<String> {
    shell::sh(
        // Unquoted paths: by default git escapes non-ASCII names ("caf\303\251"),
        // which then match no file.
        &format!("git -c core.quotePath=false {args}"),
        Some(Path::new(cwd)),
        None,
        Duration::from_secs(30),
    )?
    .into_stdout("git")
}

/// The top-level folder of the repo or worktree containing `dir`, if any.
pub fn toplevel(dir: &Path) -> Option<String> {
    // The path may be a file, or gone (deleted, or a worktree since removed).
    let mut d = dir;
    while !d.is_dir() {
        d = d.parent()?;
    }
    let top = git(&d.display().to_string(), "rev-parse --show-toplevel").ok()?;
    let top = top.trim();
    (!top.is_empty()).then(|| top.to_string())
}

/// The repos an agent worked in, most recently touched first. `cwd` is where
/// it started; `transcripts` are its session and run logs (JSON lines).
/// `shared` are folders in repos many agents work in, like the plans folder:
/// an agent's view of such a repo is only the parts it worked in.
pub fn repos(cwd: &str, transcripts: &[PathBuf], shared: &[PathBuf]) -> Vec<Repo> {
    let mut paths = vec![PathBuf::from(cwd)];
    for t in transcripts {
        touched(t, cwd, &mut paths);
    }
    roots(cwd, transcripts)
        .into_iter()
        .map(|root| {
            let scope = if shared.iter().any(|s| under(s, &root).is_some()) {
                scope_in(&root, &paths)
            } else {
                vec![]
            };
            repo(&root, cwd, scope)
        })
        .collect()
}

/// `path` relative to `root`, if it's in it. git reports real paths
/// (/private/tmp on macOS), so the real form counts too.
fn under(path: &Path, root: &str) -> Option<String> {
    let rel = |p: &Path| Some(p.strip_prefix(root).ok()?.display().to_string());
    rel(path).or_else(|| rel(&std::fs::canonicalize(path).ok()?))
}

/// The parts of `root` an agent worked in, without ones inside another.
/// Working in the repo's top folder itself means all of it: none.
fn scope_in(root: &str, paths: &[PathBuf]) -> Vec<String> {
    let mut scope: Vec<String> = paths.iter().filter_map(|p| under(p, root)).collect();
    if scope.iter().any(|s| s.is_empty()) {
        return vec![];
    }
    scope.sort();
    scope.dedup();
    let inside = |s: &String, of: &String| s != of && s.starts_with(&format!("{of}/"));
    let all = scope.clone();
    scope.retain(|s| !all.iter().any(|o| inside(s, o)));
    scope
}

fn in_scope(path: &str, scope: &[String]) -> bool {
    scope.is_empty()
        || scope
            .iter()
            .any(|s| path == s || path.starts_with(&format!("{s}/")))
}

/// The top-level folders of the repos an agent worked in, most recently
/// touched first, without reading their changes.
pub fn roots(cwd: &str, transcripts: &[PathBuf]) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let mut add = |dir: &Path| {
        if let Some(top) = toplevel(dir) {
            roots.retain(|r| r != &top);
            roots.push(top);
        }
    };
    add(Path::new(cwd));
    let mut dirs: Vec<PathBuf> = Vec::new();
    for t in transcripts {
        touched(t, cwd, &mut dirs);
    }
    // Check each folder once, keeping the order of its last use.
    let mut seen: Vec<PathBuf> = Vec::new();
    for d in dirs.into_iter().rev() {
        if !seen.contains(&d) {
            seen.push(d);
        }
    }
    for d in seen.iter().rev() {
        add(d);
    }
    roots.into_iter().rev().take(MAX_REPOS).collect()
}

fn repo(root: &str, cwd: &str, scope: Vec<String>) -> Repo {
    let base = base(root);
    let mut changes = changes(root, &base).unwrap_or_default();
    changes.retain(|c| in_scope(&c.path, &scope));
    let mut html: Vec<String> = changes
        .iter()
        .filter(|c| c.is_html && c.status != "D")
        .map(|c| c.path.clone())
        .collect();
    for f in git(root, "ls-files -- '*.html' '*.htm'")
        .unwrap_or_default()
        .lines()
        .take(200)
    {
        if !html.iter().any(|h| h == f) && in_scope(f, &scope) {
            html.push(f.to_string());
        }
    }
    Repo {
        label: label(root, cwd),
        summary: summary(root, &base),
        base: base.name,
        root: root.to_string(),
        changes,
        html,
        scope,
    }
}

fn label(root: &str, cwd: &str) -> String {
    let p = Path::new(root);
    // git reports real paths (/private/tmp on macOS), so compare both forms.
    let real = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    for under in [Path::new(cwd), real.as_path()] {
        if let Ok(rel) = p.strip_prefix(under) {
            let rel = rel.display().to_string();
            return if rel.is_empty() { ".".into() } else { rel };
        }
    }
    match p.strip_prefix(user_home()) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => root.to_string(),
    }
}

/// What a repo's changes are measured against.
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    /// e.g. `origin/main`; `HEAD` when there's no default branch to compare to.
    pub name: String,
    /// The commit the branch left it at, or `HEAD`.
    pub commit: String,
}

/// The default branch (`origin/HEAD`, else `origin/main`, `origin/master`,
/// `main`, `master`) and where this branch left it.
pub fn base(root: &str) -> Base {
    let head = Base {
        name: "HEAD".into(),
        commit: "HEAD".into(),
    };
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(r) = git(root, "rev-parse --abbrev-ref origin/HEAD") {
        let r = r.trim();
        if !r.is_empty() && r != "origin/HEAD" {
            candidates.push(r.to_string());
        }
    }
    candidates.extend(["origin/main", "origin/master", "main", "master"].map(String::from));
    let Some(name) = candidates.into_iter().find(|c| {
        git(
            root,
            &format!("rev-parse --verify --quiet {}^{{commit}}", quote(c)),
        )
        .is_ok()
    }) else {
        return head;
    };
    match git(root, &format!("merge-base HEAD {}", quote(&name))) {
        Ok(mb) if !mb.trim().is_empty() => Base {
            name,
            commit: mb.trim().to_string(),
        },
        _ => head,
    }
}

pub fn changes(root: &str, base: &Base) -> Result<Vec<Change>> {
    let pending: Vec<String> = git(root, "status --porcelain=v1 --untracked-files=all")?
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| porcelain_path(&l[3..]))
        .collect();
    let mut out: Vec<Change> = Vec::new();
    // Committed on the branch and uncommitted, against where the branch began.
    let tracked = git(
        root,
        &format!("diff -M --name-status {}", quote(&base.commit)),
    )?;
    for l in tracked.lines() {
        let mut parts = l.split('\t');
        let code = parts.next().unwrap_or_default();
        let Some(path) = parts.next_back() else {
            continue;
        };
        out.push(change(path, &code[..1.min(code.len())], &pending));
    }
    for path in git(root, "ls-files --others --exclude-standard")?.lines() {
        out.push(change(path, "??", &pending));
    }
    Ok(out)
}

fn change(path: &str, status: &str, pending: &[String]) -> Change {
    let lower = path.to_lowercase();
    Change {
        path: path.to_string(),
        status: status.to_string(),
        uncommitted: pending.iter().any(|p| p == path),
        is_html: lower.ends_with(".html") || lower.ends_with(".htm"),
    }
}

fn porcelain_path(s: &str) -> String {
    s.rsplit(" -> ")
        .next()
        .unwrap_or(s)
        .trim_matches('"')
        .to_string()
}

/// Unified diff against the base for one file, or everything when `path` is
/// empty. Untracked files are shown as all-new.
/// One file's diff, or with an empty `path` all of them: in `scope` only
/// when it's set (see `Repo::scope`).
pub fn diff(root: &str, path: &str, scope: &[String]) -> Result<String> {
    if toplevel(Path::new(root)).as_deref() != Some(root) {
        return Err(anyhow!("{root} is not a git repo"));
    }
    let base = base(root);
    let since = quote(&base.commit);
    if path.is_empty() {
        let only: String = scope.iter().map(|s| format!(" {}", quote(s))).collect();
        let only = if only.is_empty() {
            String::new()
        } else {
            format!(" --{only}")
        };
        let mut out =
            git(root, &format!("diff -M --stat --patch {since}{only}")).unwrap_or_default();
        for p in git(root, "ls-files --others --exclude-standard")?.lines() {
            if in_scope(p, scope) {
                out += &new_file_diff(root, p);
            }
        }
        return Ok(out);
    }
    let untracked = git(
        root,
        &format!("ls-files --others --exclude-standard -- {}", quote(path)),
    )?;
    if !untracked.trim().is_empty() {
        return Ok(new_file_diff(root, path));
    }
    git(root, &format!("diff -M {since} -- {}", quote(path)))
}

fn new_file_diff(root: &str, path: &str) -> String {
    let full = Path::new(root).join(path);
    match std::fs::read_to_string(&full) {
        Ok(text) => {
            let mut out =
                format!("diff --git a/{path} b/{path}\nnew file\n--- /dev/null\n+++ b/{path}\n");
            for line in text.lines().take(2000) {
                out += &format!("+{line}\n");
            }
            out
        }
        Err(_) => format!("diff --git a/{path} b/{path}\n(binary or unreadable new file)\n"),
    }
}

/// Branch, commits ahead of the base, and the last commit.
fn summary(root: &str, base: &Base) -> String {
    let branch = git(root, "rev-parse --abbrev-ref HEAD").unwrap_or_default();
    let last = git(root, "log -1 --format=%h\\ %s").unwrap_or_default();
    let mut parts = vec![branch.trim().to_string()];
    if base.name != "HEAD" {
        let ahead = git(
            root,
            &format!("rev-list --count {}..HEAD", quote(&base.commit)),
        )
        .unwrap_or_default();
        let ahead = ahead.trim();
        if ahead != "0" && !ahead.is_empty() {
            parts.push(format!("{ahead} ahead of {}", base.name));
        }
    }
    parts.push(last.trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

// -- what an agent touched ----------------------------------------------------

/// Folders an agent worked in, from one transcript: the files it edited, the
/// folders it `cd`'d into or ran `git -C` on, the worktrees it added, and the
/// working directory Claude Code recorded.
fn touched(transcript: &Path, cwd: &str, out: &mut Vec<PathBuf>) {
    let Ok(file) = std::fs::File::open(transcript) else {
        return;
    };
    let mut here = PathBuf::from(cwd);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // Most lines are text and tool output; only parse what can matter.
        if !line.contains("\"tool_use\"")
            && !line.contains("\"cwd\"")
            && !line.contains("\"item.completed\"")
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Codex: the commands it ran and the files it changed.
        if v["type"] == "item.completed" {
            let item = &v["item"];
            if let Some(cmd) = item["command"].as_str() {
                for d in command_dirs(cmd) {
                    out.push(resolve(&here, &d));
                }
            }
            for c in item["changes"].as_array().into_iter().flatten() {
                if let Some(f) = c["path"].as_str() {
                    out.push(resolve(&here, f));
                }
            }
            continue;
        }
        if let Some(c) = v["cwd"].as_str() {
            here = PathBuf::from(c);
            out.push(here.clone());
        }
        let Some(parts) = v["message"]["content"].as_array() else {
            continue;
        };
        for p in parts.iter().filter(|p| p["type"] == "tool_use") {
            let input = &p["input"];
            for key in ["file_path", "notebook_path"] {
                if let Some(f) = input[key].as_str() {
                    out.push(resolve(&here, f));
                }
            }
            if let Some(cmd) = input["command"].as_str() {
                for d in command_dirs(cmd) {
                    out.push(resolve(&here, &d));
                }
            }
        }
    }
}

fn resolve(here: &Path, p: &str) -> PathBuf {
    let p = if p == "~" {
        user_home()
    } else if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("$HOME/")) {
        user_home().join(rest)
    } else {
        PathBuf::from(p)
    };
    if p.is_absolute() {
        p
    } else {
        here.join(p)
    }
}

/// Folders a shell command works in: `cd X`, `git -C X`, `git worktree add X`.
/// A script passed to a shell (`bash -lc '…'`, as Codex runs commands) is
/// read too.
pub fn command_dirs(cmd: &str) -> Vec<String> {
    let words = words(cmd);
    if let [shell, flag, script, ..] = words.as_slice() {
        let shell = shell.rsplit('/').next().unwrap_or(shell);
        if matches!(shell, "sh" | "bash" | "zsh") && flag.starts_with('-') && flag.ends_with('c') {
            return command_dirs(script);
        }
    }
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let next = words.get(i + 1);
        match w.as_str() {
            "cd" | "pushd" => {
                if let Some(n) = next.filter(|n| !n.starts_with('-') && !is_separator(n)) {
                    out.push(n.clone());
                }
            }
            "-C" if i > 0 && words[..i].iter().rev().any(|p| p == "git") => {
                if let Some(n) = next {
                    out.push(n.clone());
                }
            }
            "worktree" if next.map(String::as_str) == Some("add") => {
                // git worktree add [-b <branch>] [--flags] <path> [<commit>]
                let mut j = i + 2;
                while let Some(a) = words.get(j) {
                    if is_separator(a) {
                        break;
                    }
                    if matches!(a.as_str(), "-b" | "-B" | "--reason") {
                        j += 2;
                    } else if a.starts_with('-') {
                        j += 1;
                    } else {
                        out.push(a.clone());
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn is_separator(w: &str) -> bool {
    matches!(w, "&&" | "||" | ";" | "|" | "&")
}

/// Split a command into words, roughly as a shell would: quotes group, and
/// `&&`, `||`, `;` and `|` are words of their own.
fn words(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                c if c.is_whitespace() => flush(&mut cur, &mut out),
                ';' | '(' | ')' => {
                    flush(&mut cur, &mut out);
                    out.push(";".into());
                }
                '&' | '|' => {
                    flush(&mut cur, &mut out);
                    let mut op = c.to_string();
                    if chars.peek() == Some(&c) {
                        op.push(chars.next().unwrap());
                    }
                    out.push(op);
                }
                _ => cur.push(c),
            },
        }
    }
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_folders_in_commands() {
        assert_eq!(
            command_dirs("cd /src/ops-apps-SRE-1 && git fetch origin && git push -u origin x"),
            ["/src/ops-apps-SRE-1"]
        );
        assert_eq!(
            command_dirs(
                "git -C ops-apps worktree add -b fix/SRE-3253 ../ops-apps-SRE-3253 origin/main"
            ),
            ["ops-apps", "../ops-apps-SRE-3253"]
        );
        assert_eq!(command_dirs("cd 'my repo'; git status"), ["my repo"]);
        assert!(command_dirs("echo cd; ls -C").is_empty());
        // How Codex runs commands.
        assert_eq!(
            command_dirs("/bin/bash -lc 'cd ../web && git status'"),
            ["../web"]
        );
    }

    #[test]
    fn reads_repos_from_a_transcript() {
        let dir = std::env::temp_dir().join(format!("drovr-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sh = |cwd: &Path, cmd: &str| {
            let out = shell::sh(cmd, Some(cwd), None, Duration::from_secs(30)).unwrap();
            assert!(out.ok(), "{cmd}: {}", out.stderr);
        };
        // A folder above two repos; the agent works in a worktree of one.
        let top = dir.join("src");
        std::fs::create_dir_all(top.join("api")).unwrap();
        std::fs::create_dir_all(top.join("web")).unwrap();
        let api = top.join("api");
        sh(&api, "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init && echo a > a.txt && git add a.txt && git -c user.name=t -c user.email=t@t commit -q -m a");
        sh(&top.join("web"), "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init");
        sh(&api, "git worktree add -q -b fix/SRE-1 ../api-SRE-1");
        let wt = top.join("api-SRE-1");
        sh(&wt, "echo b > b.txt && git add b.txt && git -c user.name=t -c user.email=t@t commit -q -m b && echo c > a.txt && echo new > new.html");

        let log = dir.join("session.jsonl");
        let lines = [
            serde_json::json!({ "cwd": top, "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Bash", "input": { "command": "git -C api worktree add -b fix/SRE-1 ../api-SRE-1" } } ] } }),
            serde_json::json!({ "cwd": top, "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": wt.join("a.txt") } } ] } }),
        ];
        std::fs::write(&log, lines.map(|l| l.to_string()).join("\n")).unwrap();

        let found = repos(&top.display().to_string(), &[log], &[]);
        let roots: Vec<_> = found.iter().map(|r| r.label.as_str()).collect();
        // The worktree it edited in last, then the repo it ran git -C on; not `web`.
        assert_eq!(roots, ["api-SRE-1", "api"]);
        let wt_repo = &found[0];
        assert_eq!(wt_repo.base, "main");
        assert!(
            wt_repo.summary.contains("fix/SRE-1 · 1 ahead of main"),
            "{}",
            wt_repo.summary
        );
        let mut got: Vec<_> = wt_repo
            .changes
            .iter()
            .map(|c| (c.path.as_str(), c.status.as_str(), c.uncommitted))
            .collect();
        got.sort();
        // Committed on the branch (b.txt), uncommitted (a.txt), untracked (new.html).
        assert_eq!(
            got,
            [
                ("a.txt", "M", true),
                ("b.txt", "A", false),
                ("new.html", "??", true)
            ]
        );
        assert!(wt_repo.html.contains(&"new.html".to_string()));

        let root = wt_repo.root.clone();
        let all = diff(&root, "", &[]).unwrap();
        assert!(
            all.contains("+b") && all.contains("+c") && all.contains("+new"),
            "{all}"
        );
        assert!(diff(&root, "b.txt", &[]).unwrap().contains("+b"));
        assert!(diff(&top.display().to_string(), "", &[]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shows_only_its_part_of_the_plans_repo() {
        let dir = std::env::temp_dir().join(format!("drovr-review-plans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sh = |cwd: &Path, cmd: &str| {
            let out = shell::sh(cmd, Some(cwd), None, Duration::from_secs(30)).unwrap();
            assert!(out.ok(), "{cmd}: {}", out.stderr);
        };
        // ~/.claude as a repo, with two agents' work in its plans folder.
        let claude = dir.join(".claude");
        let mine = claude.join("plans/release/work/skill");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(claude.join("plans/other")).unwrap();
        sh(&claude, "git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init");
        std::fs::write(mine.join("SKILL.md"), "mine").unwrap();
        std::fs::write(claude.join("plans/release/notes.html"), "<p>mine</p>").unwrap();
        std::fs::write(claude.join("plans/other/theirs.md"), "theirs").unwrap();
        std::fs::write(claude.join("settings.json"), "{}").unwrap();
        let cwd = claude.join("plans/release");
        let log = dir.join("session.jsonl");
        std::fs::write(
            &log,
            serde_json::json!({ "cwd": cwd, "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Write", "input": { "file_path": mine.join("SKILL.md") } } ] } })
            .to_string(),
        )
        .unwrap();
        let cwd = cwd.display().to_string();
        let shared = [claude.join("plans")];

        let found = repos(&cwd, std::slice::from_ref(&log), &shared);
        assert_eq!(found.len(), 1);
        let r = &found[0];
        assert_eq!(r.scope, ["plans/release"]);
        let mut paths: Vec<_> = r.changes.iter().map(|c| c.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "plans/release/notes.html",
                "plans/release/work/skill/SKILL.md"
            ]
        );
        assert_eq!(r.html, ["plans/release/notes.html"]);
        let all = diff(&r.root, "", &r.scope).unwrap();
        assert!(
            all.contains("+mine") && !all.contains("theirs") && !all.contains("+{}"),
            "{all}"
        );
        // Any other repo is shown whole.
        let whole = repos(&cwd, &[log], &[]);
        assert!(whole[0].scope.is_empty() && whole[0].changes.len() == 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
