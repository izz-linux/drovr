//! Reading what a shell command would push: the repo, remote and branch of a
//! `git push`, and the title and body of a `gh pr create` or `gh pr edit`.
//! It only parses the text; nothing here runs the command.

use crate::review;
use crate::shell::{self, quote};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Add {
    #[default]
    None,
    All,
    Paths(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Body {
    #[default]
    None,
    File(String),
    Inline(String),
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    /// From `-C` or `cd`, joined in order; None: not named.
    pub dir: Option<String>,
    pub pushes: bool,
    pub remote: Option<String>,
    /// The local branch; None: the current branch.
    pub local: Option<String>,
    pub remote_branch: Option<String>,
    pub force: bool,
    pub commits_first: bool,
    pub add: Add,
    pub body: Body,
    /// The shell's folder when `gh` runs, which `git -C` doesn't change; a
    /// relative body file is read from there. None: not named.
    pub body_dir: Option<String>,
    pub base: Option<String>,
    /// The reason, shown to the owner.
    pub unpreviewable: Option<String>,
}

/// What a command pushes or opens a PR with. None: no push and no
/// `gh pr create` or `gh pr edit`.
pub fn parse(cmd: &str) -> Option<Parsed> {
    let mut scan = Scan::default();
    scan.run(cmd, 0);
    scan.found.then_some(scan.p)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WholeBranch {
    pub base: String,
    pub text: String,
    pub marked: Vec<u32>,
    pub commits: u32,
    pub files: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PrSummary {
    pub text: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Preview {
    /// Set: the card shows only this.
    pub unavailable: Option<String>,
    pub pushes: bool,
    pub root: String,
    pub branch: String,
    pub remote: String,
    pub remote_branch: String,
    /// The tip we believe GitHub has; None: a new branch.
    pub tracking: Option<String>,
    pub assumed_folder: bool,
    pub notices: Vec<String>,
    /// `api · fix/x · 2 commits, 2 files, +14 −3`
    pub summary_line: String,
    /// `a41f2c9 fix: subject`
    pub commits: Vec<String>,
    pub this_push: String,
    /// Why `this_push` is empty; None when it isn't.
    pub this_push_note: Option<String>,
    pub whole_branch: Option<WholeBranch>,
    pub uncommitted: Option<String>,
    pub uncommitted_note: Option<String>,
    pub pr_summary: Option<PrSummary>,
}

/// What `command` would push, from local refs only: nothing is fetched and
/// the command is never run. `named_cwd` is true when `cwd` is a folder the
/// command was recorded with. Failures come back in `unavailable`.
pub fn preview(command: &str, cwd: &str, named_cwd: bool) -> Preview {
    let Some(parsed) = parse(command) else {
        return Preview::unavailable(
            "drovr couldn't find a push it can preview in this command. \
             That doesn't mean it won't push.",
        );
    };
    let assumed_folder = !named_cwd && parsed.dir.is_none();
    let here = Path::new(cwd);
    let dir = match &parsed.dir {
        Some(d) => review::resolve(here, d),
        None => here.to_path_buf(),
    };
    let mut p = if let Some(why) = &parsed.unpreviewable {
        Preview::unavailable(why.clone())
    } else if !parsed.pushes {
        Preview {
            root: if dir.is_dir() {
                review::toplevel(&dir).unwrap_or_default()
            } else {
                String::new()
            },
            ..Preview::default()
        }
    } else {
        match this_push(&parsed, &dir) {
            Ok(p) => p,
            Err(why) => Preview::unavailable(why),
        }
    };
    p.pushes = parsed.pushes;
    p.assumed_folder = assumed_folder;
    if p.unavailable.is_none() {
        let body_dir = match &parsed.body_dir {
            Some(d) => review::resolve(here, d),
            None => here.to_path_buf(),
        };
        p.pr_summary = pr_summary(&parsed.body, &body_dir);
    }
    p
}

const DIFF_MISSING: &str =
    "The diff couldn't be read, though the summary above reports changes.";

/// What the card says when `this_push` has no text.
fn this_push_note(commits: usize, files: usize, text: &str) -> Option<&'static str> {
    if !text.trim().is_empty() {
        None
    } else if files > 0 {
        Some(DIFF_MISSING)
    } else if commits > 0 {
        Some("These commits have no file changes.")
    } else {
        Some("Nothing new to push.")
    }
}

const MAX_SUMMARY: u64 = 256 * 1024;

fn pr_summary(body: &Body, dir: &Path) -> Option<PrSummary> {
    let (text, note) = match body {
        Body::None => return None,
        Body::Inline(t) => (Some(t.clone()), None),
        Body::Unreadable(why) => (None, Some(why.clone())),
        Body::File(f) => match read_summary(&review::resolve(dir, f)) {
            Ok(t) => (Some(t), None),
            Err(note) => (None, Some(note)),
        },
    };
    Some(PrSummary { text, note })
}

/// Checks the file's type and size first, so a FIFO or a huge file is
/// never read.
fn read_summary(path: &Path) -> std::result::Result<String, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("The body file doesn't exist yet.".into())
        }
        Err(e) => return Err(format!("Couldn't read the body file: {e}.")),
    };
    if !meta.is_file() {
        return Err("The body file isn't a regular file.".into());
    }
    if meta.len() > MAX_SUMMARY {
        return Err("The body file is too large to show.".into());
    }
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| format!("Couldn't read the body file: {e}."))
}

/// Whether GitHub's tip of `branch` differs from `tracking`, asking `gh`.
/// None: couldn't tell.
pub fn github_moved(gh: &str, root: &str, branch: &str, tracking: Option<&str>) -> Option<bool> {
    let path = quote(&format!("repos/{{owner}}/{{repo}}/git/ref/heads/{branch}"));
    let out = shell::sh(
        &format!("{} api {path} --jq .object.sha", quote(gh)),
        Some(Path::new(root)),
        None,
        Duration::from_secs(10),
    )
    .ok()?;
    if out.timed_out {
        return None;
    }
    if out.code == 0 {
        let tip = out.stdout.trim();
        return (!tip.is_empty()).then(|| Some(tip) != tracking);
    }
    (out.stderr.contains("Not Found") || out.stderr.contains("HTTP 404"))
        .then(|| tracking.is_some())
}

impl Preview {
    fn unavailable(why: impl Into<String>) -> Preview {
        Preview {
            unavailable: Some(why.into()),
            ..Preview::default()
        }
    }
}

fn git(root: &str, args: &[&str]) -> Result<String> {
    let args: Vec<String> = args.iter().map(|a| quote(a)).collect();
    shell::sh(
        &format!(
            "GIT_NO_LAZY_FETCH=1 git -c core.quotePath=false -c core.fsmonitor=false \
             -c log.showSignature=false -c core.hooksPath=/dev/null \
             -c diff.autoRefreshIndex=false -c color.ui=false {}",
            args.join(" ")
        ),
        Some(Path::new(root)),
        None,
        Duration::from_secs(30),
    )?
    .into_stdout("git")
}

/// Never runs a diff driver, a text conversion or a filter program from
/// the repo's config, or looks inside a submodule, whose own config could
/// do the same. A submodule bump still shows as `Subproject commit` lines.
fn diff_args() -> [&'static str; 6] {
    [
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
        "--submodule=short",
    ]
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// `(files, added, removed)` from `git diff --shortstat`.
fn shortstat(text: &str) -> (usize, usize, usize) {
    let mut out = (0, 0, 0);
    for part in text.split(',') {
        let Some(n) = part.split_whitespace().next().and_then(|n| n.parse().ok()) else {
            continue;
        };
        if part.contains("file") {
            out.0 = n;
        } else if part.contains("insertion") {
            out.1 = n;
        } else if part.contains("deletion") {
            out.2 = n;
        }
    }
    out
}

/// `(old, new)` line counts from a hunk header `@@ -a,b +c,d @@`, and the
/// first new-file line number `c`.
fn hunk_header(line: &str) -> Option<(u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split(" @@").next()?;
    let count = |r: &str| -> Option<u32> {
        match r.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => r.parse::<u32>().ok().map(|_| 1),
        }
    };
    let start = new.split(',').next()?.parse().ok()?;
    Some((count(old)?, count(new)?, start))
}

/// The path from a `+++ b/path` header. None: `/dev/null`, or a name git
/// quoted, which we leave unmarked rather than guess at.
fn header_path(line: &str) -> Option<String> {
    let p = line.strip_prefix("+++ ")?.trim_end_matches('\t');
    if p == "/dev/null" || p.starts_with('"') {
        return None;
    }
    Some(p.strip_prefix("b/").unwrap_or(p).to_string())
}

/// Calls `f(index, path, new_line)` for each added line of `diff`, where
/// `index` is 0-based within the text. Hunk counts say where a hunk ends,
/// so a body line like `+++ x` isn't taken for a file header.
fn each_added(diff: &str, mut f: impl FnMut(usize, &str, u32)) {
    let mut path: Option<String> = None;
    let (mut old_left, mut new_left, mut next) = (0u32, 0u32, 0u32);
    for (i, line) in diff.lines().enumerate() {
        if old_left == 0 && new_left == 0 {
            if line.starts_with("diff ") {
                path = None;
            } else if line.starts_with("+++ ") {
                path = header_path(line);
            } else if let Some((o, n, start)) = hunk_header(line) {
                (old_left, new_left, next) = (o, n, start);
            }
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                new_left = new_left.saturating_sub(1);
                if let Some(p) = &path {
                    f(i, p, next);
                }
                next += 1;
            }
            Some(b'-') => old_left = old_left.saturating_sub(1),
            Some(b'\\') => {}
            _ => {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                next += 1;
            }
        }
    }
}

/// path -> new-file line numbers of the `+` lines in `diff`.
fn added_lines(diff: &str) -> HashMap<String, HashSet<u32>> {
    let mut out: HashMap<String, HashSet<u32>> = HashMap::new();
    each_added(diff, |_, path, n| {
        out.entry(path.to_string()).or_default().insert(n);
    });
    out
}

/// The 0-based lines of `branch_diff` that add a line in `added`. Both
/// diffs end at the same tip, so a line number names the same line in each.
fn mark(branch_diff: &str, added: &HashMap<String, HashSet<u32>>) -> Vec<u32> {
    let mut out = Vec::new();
    each_added(branch_diff, |i, path, n| {
        if added.get(path).is_some_and(|set| set.contains(&n)) {
            out.push(i as u32);
        }
    });
    out
}

/// The branch's base: `--base X` as `<remote>/X`, else `X`, else the
/// default branch, with a notice when `X` can't be found.
fn branch_base(
    root: &str,
    parsed: &Parsed,
    remote: &str,
    local_ref: &str,
    notices: &mut Vec<String>,
) -> Option<(String, String)> {
    let verify = |c: &str| {
        git(
            root,
            &["rev-parse", "--verify", "-q", &format!("{c}^{{commit}}")],
        )
        .is_ok()
    };
    let merge_base = |name: &str| {
        git(root, &["merge-base", local_ref, name])
            .ok()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
    };
    if let Some(x) = parsed.base.as_deref().filter(|x| !x.starts_with('-')) {
        let found = [format!("{remote}/{x}"), x.to_string()]
            .into_iter()
            .filter(|c| verify(c))
            .find_map(|c| merge_base(&c).map(|m| (c, m)));
        if found.is_some() {
            return found;
        }
    }
    let default = review::base_of(root, local_ref);
    if let Some(x) = &parsed.base {
        notices.push(if default.name == "HEAD" {
            format!("The base branch {x} isn't known here, so there's no branch to compare with.")
        } else {
            format!(
                "The base branch {x} isn't known here; comparing with {} instead.",
                default.name
            )
        });
    }
    (default.name != "HEAD").then_some((default.name, default.commit))
}

fn this_push(parsed: &Parsed, dir: &Path) -> std::result::Result<Preview, String> {
    if !dir.is_dir() {
        return Err(format!("The folder {} doesn't exist.", dir.display()));
    }
    let root = review::toplevel(dir)
        .ok_or_else(|| format!("{} isn't in a git repository.", dir.display()))?;
    let current = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());
    let on_head = |b: &str| current.as_deref() == Some(b);
    let checked_out = match &parsed.local {
        Some(l) => on_head(l),
        None => current.is_some(),
    };
    let branch = match (&parsed.local, current.clone()) {
        (Some(l), _) => l.clone(),
        (None, Some(c)) => c,
        (None, None) => return Err("HEAD isn't on a branch, so there's nothing to compare.".into()),
    };
    let local_ref = format!("refs/heads/{branch}");
    if branch.starts_with('-') || git(&root, &["rev-parse", "--verify", "-q", &local_ref]).is_err()
    {
        return Err(format!("{branch} isn't a local branch here."));
    }
    let remotes = git(&root, &["remote"]).unwrap_or_default();
    let remote = match &parsed.remote {
        Some(r) => r.clone(),
        None => git(
            &root,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )
        .map(|r| r.trim().to_string())
        .ok()
        .filter(|r| !r.is_empty() && r != ".")
        .unwrap_or_else(|| "origin".into()),
    };
    if remote.starts_with('-') || !remotes.lines().any(|r| r == remote) {
        return Err(format!("{remote} isn't a remote of this repository."));
    }
    let remote_branch = parsed
        .remote_branch
        .clone()
        .unwrap_or_else(|| branch.clone());
    if remote_branch.starts_with('-') {
        return Err(format!("{remote_branch} isn't a usable branch name."));
    }

    let tracking = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/remotes/{remote}/{remote_branch}"),
        ],
    )
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());
    let start = match &tracking {
        Some(t) => t.clone(),
        None => {
            let base = review::base_of(&root, &local_ref);
            if base.name == "HEAD" {
                return Err("There's no default branch to compare this new branch with.".into());
            }
            base.commit
        }
    };

    let mut notices = Vec::new();
    if let Some(t) = &tracking {
        if git(&root, &["merge-base", "--is-ancestor", t, &local_ref]).is_err() {
            notices.push(if parsed.force {
                "History is being rewritten; this diff may include changes from main.".to_string()
            } else {
                format!(
                    "GitHub's copy of {remote_branch} isn't an ancestor of {branch}; \
                     a push without --force will be rejected."
                )
            });
        }
    }

    let range = format!("{start}..{local_ref}");
    let commits: Vec<String> = git(&root, &["log", "--format=%h %s", &range])
        .map_err(|e| e.to_string())?
        .lines()
        .map(str::to_string)
        .collect();
    let mut diff = vec!["diff"];
    diff.extend(diff_args());
    diff.extend([start.as_str(), local_ref.as_str(), "--"]);
    let this_push = git(&root, &diff).map_err(|e| e.to_string())?;
    diff.insert(1, "--shortstat");
    let (files, added, removed) = shortstat(&git(&root, &diff).map_err(|e| e.to_string())?);

    let name = Path::new(&root)
        .file_name()
        .map_or(root.clone(), |n| n.to_string_lossy().into_owned());
    let this_push_note = this_push_note(commits.len(), files, &this_push).map(str::to_string);
    let summary_line = if commits.is_empty() && this_push.is_empty() && files == 0 {
        format!("{name} · {branch} · nothing to push")
    } else {
        format!(
            "{name} · {branch} · {}, {}, +{added} \u{2212}{removed}",
            plural(commits.len(), "commit"),
            plural(files, "file"),
        )
    };
    let mut summary_line = summary_line;
    let (mut pending, mut pending_note) = (None, None);
    if parsed.commits_first && checked_out {
        if repo_has_filter(&root) {
            pending_note = Some(UNCOMMITTED_FILTERED.to_string());
        } else {
            match uncommitted(&root, &parsed.add) {
                Ok((text, files)) if files > 0 => {
                    summary_line +=
                        &format!("; plus uncommitted changes in {}", plural(files, "file"));
                    pending = Some(text);
                }
                Ok(_) => {}
                Err(e) => pending_note = Some(format!("Uncommitted changes couldn't be read: {e}")),
            }
        }
    }
    let whole_branch = if tracking.is_some() {
        whole_branch(&root, parsed, &remote, &local_ref, &this_push, &mut notices)
    } else {
        None
    };
    Ok(Preview {
        root,
        branch,
        remote,
        remote_branch,
        tracking,
        notices,
        summary_line,
        commits,
        this_push,
        this_push_note,
        whole_branch,
        uncommitted: pending,
        uncommitted_note: pending_note,
        ..Preview::default()
    })
}

/// The branch against its base with this push's lines marked; None when
/// the branch has nothing beyond its base or the base can't be found.
fn whole_branch(
    root: &str,
    parsed: &Parsed,
    remote: &str,
    local_ref: &str,
    this_push: &str,
    notices: &mut Vec<String>,
) -> Option<WholeBranch> {
    let (base, start) = branch_base(root, parsed, remote, local_ref, notices)?;
    let range = format!("{start}..{local_ref}");
    let commits: u32 = git(root, &["rev-list", "--count", &range])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if commits == 0 {
        return None;
    }
    let mut diff = vec!["diff"];
    diff.extend(diff_args());
    diff.extend([start.as_str(), local_ref, "--"]);
    let text = git(root, &diff).ok()?;
    diff.insert(1, "--shortstat");
    let (files, _, _) = shortstat(&git(root, &diff).ok()?);
    let marked = mark(&text, &added_lines(this_push));
    Some(WholeBranch {
        base,
        text,
        marked,
        commits,
        files: files as u32,
    })
}

const FILTER_CHECK: &str = "git config --local --includes --get-regexp '^filter\\.'";
const FILTER_CHECK_WORKTREE: &str = "git config --worktree --includes --get-regexp '^filter\\.'";

/// True when the repo's own config defines a content filter, or when that
/// can't be told. Global and system config are not read: a global
/// `filter.lfs` was set by the owner, not an agent. `--worktree` is asked
/// only when `extensions.worktreeConfig` is on, because git errors on it
/// otherwise in a repo with several worktrees.
fn repo_has_filter(root: &str) -> bool {
    let run = |cmd: &str| shell::sh(cmd, Some(Path::new(root)), None, Duration::from_secs(10));
    let none = |cmd: &str| {
        run(cmd).is_ok_and(|o| !o.timed_out && o.code == 1 && o.stdout.trim().is_empty())
    };
    if !none(FILTER_CHECK) {
        return true;
    }
    let ext = run("git config --local --type=bool --get extensions.worktreeConfig");
    match ext {
        Ok(o) if !o.timed_out && o.code == 1 && o.stdout.trim().is_empty() => false,
        Ok(o) if o.ok() && o.stdout.trim() == "false" => false,
        Ok(o) if o.ok() && o.stdout.trim() == "true" => !none(FILTER_CHECK_WORKTREE),
        _ => true,
    }
}

const UNCOMMITTED_FILTERED: &str =
    "Uncommitted changes can't be previewed in this repo because its git config defines content filters.";

/// What the command's `git add` and `git commit` would include: `(text,
/// files)`. Tracked changes come from `diff HEAD`; untracked files only
/// when `add` names them.
fn uncommitted(root: &str, add: &Add) -> Result<(String, usize)> {
    let mut diff = vec!["diff"];
    diff.extend(diff_args());
    diff.extend(["HEAD", "--"]);
    let mut pathspec: Vec<&str> = Vec::new();
    if let Add::Paths(paths) = add {
        pathspec.extend(paths.iter().map(String::as_str));
        diff.extend(&pathspec);
    }
    let mut text = git(root, &diff)?;
    if !matches!(add, Add::None) {
        let mut ls = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
        ls.extend(&pathspec);
        for path in git(root, &ls)?.split('\0').filter(|p| !p.is_empty()) {
            let full = Path::new(root).join(path);
            if full
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                text += &format!("diff --git a/{path} b/{path}\n(symlink)\n");
            } else {
                text += &review::new_file_diff(root, path);
            }
        }
    }
    let files = text
        .lines()
        .filter(|l| l.starts_with("diff --git "))
        .count();
    Ok((text, files))
}

const MAX_DEPTH: usize = 4;

const WRITES_BODY: &str = "The command writes this file first.";

/// Starts a word `strip_redirects` leaves where an output redirect was: the
/// file it writes.
const WRITE_MARK: &str = "\0>";

/// `gh pr` flags that take a value we don't read.
const GH_SKIPPED: &[&str] = &[
    "--title",
    "-t",
    "--label",
    "-l",
    "--assignee",
    "-a",
    "--reviewer",
    "-r",
    "--milestone",
    "-m",
    "--project",
    "-p",
    "--template",
    "-T",
    "--repo",
    "-R",
    "--recover",
];

const GH_READ: &[&str] = &[
    "--body-file",
    "-F",
    "--body",
    "-b",
    "--base",
    "-B",
    "--head",
    "-H",
];

#[derive(Default)]
struct Scan {
    p: Parsed,
    found: bool,
    pushed: bool,
    /// The shell's folder, from `cd`. `git -C` doesn't change it.
    cwd: Option<String>,
    cwd_unknown: bool,
    committed: bool,
    add: Add,
    add_unknown: bool,
    /// Files redirected or `tee`d to so far.
    writes: HashSet<String>,
}

impl Scan {
    fn refuse(&mut self, why: impl Into<String>) {
        if self.p.unpreviewable.is_none() {
            self.p.unpreviewable = Some(why.into());
        }
    }

    fn run(&mut self, cmd: &str, depth: usize) {
        let words = strip_redirects(tokens(cmd));
        for seg in words.split(|w| is_separator(w)) {
            self.segment(seg, depth);
        }
    }

    fn segment(&mut self, seg: &[String], depth: usize) {
        for target in seg.iter().filter_map(|w| w.strip_prefix(WRITE_MARK)) {
            self.wrote(target);
        }
        let seg: Vec<String> = seg
            .iter()
            .filter(|w| !w.starts_with(WRITE_MARK))
            .cloned()
            .collect();
        let Some((prog, args)) = command(&seg).split_first() else {
            return;
        };
        match prog.rsplit('/').next().unwrap_or(prog) {
            "sh" | "bash" | "zsh" if depth < MAX_DEPTH => {
                let flag = args
                    .iter()
                    .position(|a| a.starts_with('-') && !a.starts_with("--") && a.ends_with('c'));
                if let Some(script) = flag.and_then(|i| args.get(i + 1)) {
                    self.run(script, depth + 1);
                }
            }
            "cd" | "pushd" => self.cd(args),
            "tee" => {
                for target in args.iter().filter(|a| !a.starts_with('-')) {
                    self.wrote(target);
                }
            }
            "git" => self.git(args),
            "gh" => self.gh(args),
            _ => {}
        }
    }

    /// A file's path as written in the command, from the shell's folder.
    fn path(&self, file: &str) -> String {
        join(
            self.cwd.as_deref(),
            file.strip_prefix("./").unwrap_or(file),
        )
    }

    fn wrote(&mut self, file: &str) {
        let path = self.path(file);
        self.writes.insert(path);
    }

    fn cd(&mut self, args: &[String]) {
        let Some(d) = args.iter().find(|a| !a.starts_with('-')) else {
            return;
        };
        if unknowable(d) {
            self.cwd_unknown = true;
            return;
        }
        if is_absolute(d) {
            self.cwd_unknown = false;
        }
        self.cwd = Some(join(self.cwd.as_deref(), d));
    }

    fn git(&mut self, args: &[String]) {
        let mut dir = self.cwd.clone();
        let mut dir_unknown = self.cwd_unknown;
        let mut elsewhere = false;
        let mut i = 0;
        while let Some(a) = args.get(i) {
            match a.as_str() {
                "-C" => {
                    match args.get(i + 1) {
                        Some(d) if unknowable(d) => dir_unknown = true,
                        Some(d) => dir = Some(join(dir.as_deref(), d)),
                        None => {}
                    }
                    i += 2;
                }
                "-c" => i += 2,
                "--git-dir" | "--work-tree" => {
                    elsewhere = true;
                    i += 2;
                }
                a if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => {
                    elsewhere = true;
                    i += 1;
                }
                a if a.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some((sub, rest)) = args.get(i..).and_then(|a| a.split_first()) else {
            return;
        };
        match sub.as_str() {
            "push" => {
                if elsewhere {
                    self.refuse("This push sets --git-dir or --work-tree.");
                }
                self.push(rest, dir, dir_unknown);
            }
            "add" => self.add(rest),
            "commit" => self.committed = true,
            _ => {}
        }
    }

    fn add(&mut self, args: &[String]) {
        let mut paths = Vec::new();
        for a in args {
            match a.as_str() {
                "-A" | "--all" | "-u" | "--update" | "." => self.add = Add::All,
                a if a.starts_with('-') => {}
                a if unknowable(a) => self.add_unknown = true,
                a => paths.push(a.to_string()),
            }
        }
        if paths.is_empty() {
            return;
        }
        match &mut self.add {
            Add::All => {}
            Add::Paths(have) => have.extend(paths),
            add => *add = Add::Paths(paths),
        }
    }

    fn found(&mut self, dir_unknown: bool) {
        self.found = true;
        if dir_unknown || self.cwd_unknown {
            self.refuse("The folder is set by a shell variable, so it can't be read.");
        }
    }

    fn push(&mut self, args: &[String], dir: Option<String>, dir_unknown: bool) {
        if self.pushed {
            self.refuse("This command pushes more than once.");
            return;
        }
        self.pushed = true;
        self.found(dir_unknown);
        self.p.pushes = true;
        self.p.dir = dir;
        self.commits_first();

        let mut positional: Vec<&str> = Vec::new();
        let mut j = 0;
        while let Some(a) = args.get(j) {
            let a = a.as_str();
            match a {
                "--tags" | "--all" | "--mirror" | "--delete" | "-d" => {
                    self.refuse(format!("git push {a} isn't previewed."));
                }
                "-f" | "--force" | "--force-with-lease" | "--force-if-includes" => {
                    self.p.force = true
                }
                "-o" | "--push-option" | "--receive-pack" | "--exec" => j += 1,
                "--repo" => {
                    self.refuse("This push names its repository with --repo.");
                    j += 1;
                }
                a if a.starts_with("--repo=") => {
                    self.refuse("This push names its repository with --repo.");
                }
                a if a.starts_with("--force-with-lease=") => self.p.force = true,
                a if a.starts_with("--") => {}
                a if a.starts_with('-') && a.len() > 1 => {
                    if a.contains('f') {
                        self.p.force = true;
                    }
                    if a.contains('d') {
                        self.refuse("git push --delete isn't previewed.");
                    }
                }
                a => positional.push(a),
            }
            j += 1;
        }

        if positional.iter().any(|w| unknowable(w)) {
            self.refuse("The remote or branch is set by a shell variable, so it can't be read.");
            return;
        }
        let Some((remote, refspecs)) = positional.split_first() else {
            return;
        };
        self.p.remote = Some(remote.to_string());
        match refspecs {
            [] => {}
            [spec] => self.refspec(spec),
            _ => self.refuse("This push names more than one branch."),
        }
    }

    fn refspec(&mut self, spec: &str) {
        let spec = match spec.strip_prefix('+') {
            Some(rest) => {
                self.p.force = true;
                rest
            }
            None => spec,
        };
        if spec.contains(['*', '^', '?', '[']) {
            self.refuse("This push uses a pattern for its branch.");
            return;
        }
        let branch = |b: &str| b.strip_prefix("refs/heads/").unwrap_or(b).to_string();
        let (src, dst) = match spec.split_once(':') {
            None => (spec, (spec != "HEAD").then_some(spec)),
            Some(("", _)) => {
                self.refuse("This push deletes a remote branch.");
                return;
            }
            Some((_, "")) => {
                self.refuse("This push names no branch to push to.");
                return;
            }
            Some((src, dst)) => (src, Some(dst)),
        };
        self.p.local = (src != "HEAD").then(|| branch(src));
        self.p.remote_branch = dst.map(branch);
    }

    fn commits_first(&mut self) {
        self.p.commits_first = self.committed;
        if self.committed {
            self.p.add = self.add.clone();
            if self.add_unknown {
                self.refuse("This push follows a git add of unknown files.");
            }
        }
    }

    fn gh(&mut self, args: &[String]) {
        let [pr, action, rest @ ..] = args else {
            return;
        };
        if pr != "pr" || !matches!(action.as_str(), "create" | "edit") {
            return;
        }
        let create = action == "create";
        if self.pushed {
            self.found = true;
        } else {
            self.found(false);
            self.p.dir = self.cwd.clone();
            if create {
                self.p.pushes = true;
                self.commits_first();
            }
        }
        self.p.body_dir = self.cwd.clone();
        let mut j = 0;
        while j < rest.len() {
            if let Some(v) = value(rest, &mut j, &["--body-file", "-F"]) {
                self.p.body = if v == "-" {
                    Body::Unreadable("The body comes from standard input.".into())
                } else if unknowable(v) {
                    Body::Unreadable("The body file is set by a shell variable.".into())
                } else if self.writes.contains(&self.path(v)) {
                    Body::Unreadable(WRITES_BODY.into())
                } else if self.cwd_unknown && !is_absolute(v) {
                    Body::Unreadable("The body file's folder is set by a shell variable.".into())
                } else {
                    Body::File(v.to_string())
                };
            } else if let Some(v) = value(rest, &mut j, &["--body", "-b"]) {
                self.p.body = if unknowable(v) {
                    Body::Unreadable("The body comes from a command or variable.".into())
                } else {
                    Body::Inline(v.to_string())
                };
            } else if let Some(v) = value(rest, &mut j, &["--base", "-B"]) {
                if unknowable(v) {
                    self.refuse("The base branch is set by a shell variable.");
                } else {
                    self.p.base = Some(v.to_string());
                }
            } else if let Some(v) = value(rest, &mut j, &["--head", "-H"]) {
                if unknowable(v) {
                    self.refuse("The head branch is set by a shell variable.");
                } else if create && self.p.local.is_none() {
                    self.p.local = Some(v.rsplit(':').next().unwrap_or(v).to_string());
                }
            } else {
                value(rest, &mut j, GH_SKIPPED);
            }
            j += 1;
        }
    }
}

/// The value of the flag at `rest[*j]` if it is one of `names`, as `--flag v`
/// (moving `j` onto `v`) or `--flag=v`. A flag with no value reads as "".
fn value<'a>(rest: &'a [String], j: &mut usize, names: &[&str]) -> Option<&'a str> {
    let a = rest[*j].as_str();
    for n in names {
        if a == *n {
            *j += 1;
            return Some(rest.get(*j).map_or("", String::as_str));
        }
        if n.starts_with("--") {
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v);
            }
        }
    }
    None
}

fn is_separator(w: &str) -> bool {
    matches!(w, "&&" | "||" | ";" | "|" | "&")
}

/// `NAME=value`, the environment prefix of a command.
fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The command a segment runs, past assignments and simple prefixes. A
/// prefix with its own flags (`sudo -u x`) isn't read through.
fn command(seg: &[String]) -> &[String] {
    let mut rest = seg;
    loop {
        rest = &rest[rest.iter().take_while(|w| is_assignment(w)).count()..];
        let Some(first) = rest.first() else {
            return rest;
        };
        let skip = match first.rsplit('/').next().unwrap_or(first) {
            "then" | "do" | "else" | "if" | "elif" | "while" | "until" | "!" | "{" | "time"
            | "command" | "sudo" | "env" => 1,
            "timeout"
                if rest
                    .get(1)
                    .is_some_and(|d| d.starts_with(|c: char| c.is_ascii_digit())) =>
            {
                2
            }
            _ => return rest,
        };
        rest = &rest[skip..];
    }
}

/// Words that need the shell to expand them.
fn unknowable(w: &str) -> bool {
    w.contains('$') || w.contains('`')
}

fn is_absolute(d: &str) -> bool {
    d.starts_with('/') || d.starts_with('~')
}

fn join(cur: Option<&str>, d: &str) -> String {
    match cur {
        Some(c) if !is_absolute(d) => {
            format!("{}/{d}", c.trim_end_matches('/'))
        }
        _ => d.to_string(),
    }
}

/// Drop redirections (`> f`, `2>&1`, `<<EOF`) so they aren't read as
/// arguments, marking the files they write (see `WRITE_MARK`). A word after
/// a flag that takes a value is left alone: it's the value
/// (`--body '<b>x</b>'`).
fn strip_redirects(words: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let after_flag = out
            .last()
            .is_some_and(|p| GH_READ.contains(&p.as_str()) || GH_SKIPPED.contains(&p.as_str()));
        let op = w.trim_start_matches(|c: char| c.is_ascii_digit());
        if !after_flag && op.starts_with(['<', '>']) {
            let bare = op.chars().all(|c| c == '<' || c == '>');
            let next = words.get(i + 1).map(String::as_str);
            let target = if !bare {
                i += 1;
                Some(op.trim_start_matches('>'))
            } else if next == Some("&") {
                i += 3;
                None
            } else {
                i += 2;
                next
            };
            if let Some(t) = target.filter(|_| op.starts_with('>')) {
                out.push(format!("{WRITE_MARK}{t}"));
            }
            continue;
        }
        if !after_flag {
            if let Some((_, t)) = w.split_once('>') {
                out.push(format!("{WRITE_MARK}{t}"));
            }
        }
        out.push(w.clone());
        i += 1;
    }
    out
}

/// Split a command into words as a shell would. Unlike `review::words`, this
/// understands what can hide a quote or a command: backslashes and `$(…)`
/// inside double quotes, backticks, comments, and heredoc bodies, which are
/// dropped. A `$(…)` stays in its word, so the word reads as `unknowable`.
fn tokens(cmd: &str) -> Vec<String> {
    let cs: Vec<char> = cmd.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut pending: Vec<(String, bool)> = Vec::new();
    let flush = |cur: &mut String, in_word: &mut bool, out: &mut Vec<String>| {
        if *in_word || !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
        *in_word = false;
    };
    let mut i = 0;
    while let Some(&c) = cs.get(i) {
        let next = cs.get(i + 1).copied();
        match c {
            '\'' => {
                let end = closing(&cs, i + 1, '\'');
                cur.extend(&cs[i + 1..end]);
                in_word = true;
                i = end + 1;
            }
            '"' => {
                i = double_quoted(&cs, i + 1, &mut cur);
                in_word = true;
            }
            '\\' => {
                if let Some(n) = next.filter(|n| *n != '\n') {
                    cur.push(n);
                    in_word = true;
                }
                i += 2;
            }
            '$' if next == Some('(') => {
                let end = substitution(&cs, i + 2);
                cur.extend(&cs[i..end]);
                in_word = true;
                i = end;
            }
            '$' if next == Some('{') => {
                let end = (closing(&cs, i + 2, '}') + 1).min(cs.len());
                cur.extend(&cs[i..end]);
                in_word = true;
                i = end;
            }
            '`' => {
                let end = backticks(&cs, i + 1);
                cur.extend(&cs[i..end]);
                in_word = true;
                i = end;
            }
            '#' if !in_word && cur.is_empty() => i = line_end(&cs, i),
            '\n' => {
                flush(&mut cur, &mut in_word, &mut out);
                out.push(";".into());
                i += 1;
                for (delim, dash) in pending.drain(..) {
                    i = past_heredoc(&cs, i, &delim, dash);
                }
            }
            c if c.is_whitespace() => {
                flush(&mut cur, &mut in_word, &mut out);
                i += 1;
            }
            ';' | '(' | ')' => {
                flush(&mut cur, &mut in_word, &mut out);
                out.push(";".into());
                i += 1;
            }
            '&' | '|' => {
                flush(&mut cur, &mut in_word, &mut out);
                if next == Some(c) {
                    out.push(format!("{c}{c}"));
                    i += 2;
                } else {
                    out.push(c.to_string());
                    i += 1;
                }
            }
            '<' if next == Some('<') => {
                if cs.get(i + 2) == Some(&'<') {
                    cur.push_str("<<<");
                    i += 3;
                } else if let Some((delim, dash, end)) = heredoc_delim(&cs, i + 2) {
                    flush(&mut cur, &mut in_word, &mut out);
                    out.push(format!("<<{delim}"));
                    pending.push((delim, dash));
                    i = end + 1;
                } else {
                    cur.push_str("<<");
                    i += 2;
                }
            }
            c => {
                cur.push(c);
                i += 1;
            }
        }
    }
    flush(&mut cur, &mut in_word, &mut out);
    out
}

/// The index of the next `close` from `i`, or the end.
fn closing(cs: &[char], i: usize, close: char) -> usize {
    cs[i.min(cs.len())..]
        .iter()
        .position(|c| *c == close)
        .map_or(cs.len(), |n| i + n)
}

fn line_end(cs: &[char], i: usize) -> usize {
    closing(cs, i, '\n')
}

/// Reads a double-quoted string from just after its `"` into `cur`, and
/// returns the index after the closing `"`.
fn double_quoted(cs: &[char], mut i: usize, cur: &mut String) -> usize {
    while let Some(&c) = cs.get(i) {
        match c {
            '"' => return i + 1,
            '\\' => {
                match cs.get(i + 1) {
                    Some('\n') | None => {}
                    Some(n @ ('"' | '\\' | '$' | '`')) => cur.push(*n),
                    Some(n) => {
                        cur.push('\\');
                        cur.push(*n);
                    }
                }
                i += 2;
            }
            '$' if cs.get(i + 1) == Some(&'(') => {
                let end = substitution(cs, i + 2);
                cur.extend(&cs[i..end]);
                i = end;
            }
            '`' => {
                let end = backticks(cs, i + 1);
                cur.extend(&cs[i..end]);
                i = end;
            }
            c => {
                cur.push(c);
                i += 1;
            }
        }
    }
    cs.len()
}

/// The index after the backtick closing one opened just before `i`.
fn backticks(cs: &[char], mut i: usize) -> usize {
    while let Some(&c) = cs.get(i) {
        match c {
            '\\' => i += 2,
            '`' => return i + 1,
            _ => i += 1,
        }
    }
    cs.len()
}

/// The index after the `)` closing a `$(` opened just before `i`. Quoting
/// starts afresh inside, and a heredoc's body can hold anything.
fn substitution(cs: &[char], mut i: usize) -> usize {
    let mut depth = 1;
    let mut pending: Vec<(String, bool)> = Vec::new();
    while let Some(&c) = cs.get(i) {
        match c {
            '\'' => i = closing(cs, i + 1, '\'') + 1,
            '"' => i = double_quoted(cs, i + 1, &mut String::new()),
            '\\' => i += 2,
            '`' => i = backticks(cs, i + 1),
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            '#' if cs[i - 1].is_whitespace() => i = line_end(cs, i),
            '\n' => {
                i += 1;
                for (delim, dash) in pending.drain(..) {
                    i = past_heredoc(cs, i, &delim, dash);
                }
            }
            '<' if cs.get(i + 1) == Some(&'<') => {
                if cs.get(i + 2) == Some(&'<') {
                    i += 3;
                } else if let Some((delim, dash, end)) = heredoc_delim(cs, i + 2) {
                    pending.push((delim, dash));
                    i = end + 1;
                } else {
                    i += 2;
                }
            }
            _ => i += 1,
        }
    }
    cs.len()
}

/// The index after the line equal to `delim`, reading from the start of a
/// line. With `dash` (`<<-`), leading tabs don't count.
fn past_heredoc(cs: &[char], mut i: usize, delim: &str, dash: bool) -> usize {
    while i < cs.len() {
        let end = line_end(cs, i);
        let line: String = cs[i..end].iter().collect();
        let line = if dash {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        i = end + 1;
        if line == delim {
            return i.min(cs.len());
        }
    }
    cs.len()
}

/// The delimiter after `<<` starting at `cs[m]`, if any, whether it was
/// `<<-`, and the index of its last character.
fn heredoc_delim(cs: &[char], mut m: usize) -> Option<(String, bool, usize)> {
    let dash = cs.get(m) == Some(&'-');
    if dash {
        m += 1;
    }
    while cs.get(m).is_some_and(|c| *c == ' ' || *c == '\t') {
        m += 1;
    }
    let q = cs.get(m).copied().filter(|c| *c == '\'' || *c == '"');
    if q.is_some() {
        m += 1;
    }
    let start = m;
    while let Some(c) = cs.get(m) {
        let stop = match q {
            Some(q) => *c == q,
            None => c.is_whitespace() || ";&|<>()'\"".contains(*c),
        };
        if stop {
            break;
        }
        m += 1;
    }
    if m == start {
        return None;
    }
    let delim: String = cs[start..m].iter().collect();
    let end = if q.is_some() && m < cs.len() {
        m
    } else {
        m - 1
    };
    Some((delim, dash, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    fn s2(v: &str) -> Option<&str> {
        Some(v)
    }

    fn unpreviewable(cmd: &str) {
        let p = parse(cmd).unwrap_or_else(|| panic!("no parse: {cmd}"));
        assert!(p.unpreviewable.is_some(), "should refuse: {cmd}");
    }

    #[test]
    fn parses_commands() {
        type Check = fn(&Parsed);
        let rows: Vec<(&str, Check)> = vec![
            ("git -C /w/repo push origin feat/SRE-3205-x", |p| {
                assert_eq!(p.dir, s("/w/repo"));
                assert_eq!(p.remote, s("origin"));
                assert_eq!(p.local, s("feat/SRE-3205-x"));
                assert_eq!(p.remote_branch, s("feat/SRE-3205-x"));
                assert!(p.pushes && p.unpreviewable.is_none());
            }),
            ("cd /w/repo && git push", |p| {
                assert_eq!(p.dir, s("/w/repo"));
                assert_eq!(
                    (&p.remote, &p.local, &p.remote_branch),
                    (&None, &None, &None)
                );
            }),
            ("cd /w && git -C repo push", |p| {
                assert_eq!(p.dir, s("/w/repo"))
            }),
            ("cd /w && git -C /x push", |p| assert_eq!(p.dir, s("/x"))),
            ("git -c k=v --no-pager push origin a", |p| {
                assert_eq!(p.local, s("a"))
            }),
            ("git push -u origin HEAD", |p| {
                assert_eq!(p.remote, s("origin"));
                assert_eq!((&p.local, &p.remote_branch), (&None, &None));
            }),
            ("git push origin a:b", |p| {
                assert_eq!(p.local, s("a"));
                assert_eq!(p.remote_branch, s("b"));
            }),
            ("git push origin HEAD:b", |p| {
                assert_eq!(p.local, None);
                assert_eq!(p.remote_branch, s("b"));
            }),
            ("git push origin refs/heads/a", |p| {
                assert_eq!(p.local, s("a"))
            }),
            ("git push --force-with-lease origin a", |p| assert!(p.force)),
            ("git push --force-with-lease=a:b origin a", |p| {
                assert!(p.force)
            }),
            ("git push -f origin a", |p| assert!(p.force)),
            ("git push -o ci.skip origin a", |p| {
                assert_eq!(p.remote, s("origin"));
                assert!(!p.force);
            }),
            ("git push origin +a", |p| {
                assert!(p.force);
                assert_eq!(p.local, s("a"));
            }),
            ("git push origin a 2>&1 | tail", |p| {
                assert_eq!(p.local, s("a"));
                assert!(p.unpreviewable.is_none());
            }),
            (
                "git push origin a && gh pr create --body-file pr.md --base dev",
                |p| {
                    assert_eq!(p.body, Body::File("pr.md".into()));
                    assert_eq!(p.base, s("dev"));
                    assert!(p.pushes);
                    assert_eq!(p.local, s("a"));
                },
            ),
            ("gh pr edit 7 --body-file=pr.md", |p| {
                assert!(!p.pushes);
                assert_eq!(p.body, Body::File("pr.md".into()));
            }),
            ("gh pr create -F pr.md", |p| {
                assert!(p.pushes);
                assert_eq!(p.body, Body::File("pr.md".into()));
            }),
            ("gh pr create -b 'hello'", |p| {
                assert_eq!(p.body, Body::Inline("hello".into()))
            }),
            ("gh pr create --title t --body=<b>x</b> -B main", |p| {
                assert_eq!(p.body, Body::Inline("<b>x</b>".into()));
                assert_eq!(p.base, s("main"));
            }),
            ("gh pr create --head x --body-file pr.md", |p| {
                assert!(p.pushes);
                assert_eq!(p.local, s("x"));
            }),
            ("gh pr create --head fork:x", |p| {
                assert_eq!(p.local, s("x"))
            }),
            ("gh pr create --body \"$(cat pr.md)\"", |p| {
                assert!(matches!(p.body, Body::Unreadable(_)))
            }),
            ("gh pr create --body-file -", |p| {
                assert!(matches!(p.body, Body::Unreadable(_)))
            }),
            ("gh pr create --body-file $F", |p| {
                assert!(matches!(p.body, Body::Unreadable(_)))
            }),
            ("git add -A && git commit -m x && git push", |p| {
                assert!(p.commits_first);
                assert_eq!(p.add, Add::All);
            }),
            ("git add a b && git commit -m x && git push", |p| {
                assert_eq!(p.add, Add::Paths(vec!["a".into(), "b".into()]))
            }),
            ("git add . && git commit -m x && git push", |p| {
                assert_eq!(p.add, Add::All)
            }),
            ("git commit -am x && git push", |p| {
                assert!(p.commits_first);
                assert_eq!(p.add, Add::None);
            }),
            ("git add a && git push", |p| {
                assert!(!p.commits_first);
                assert_eq!(p.add, Add::None);
            }),
            ("FOO=1 git push origin a", |p| assert_eq!(p.local, s("a"))),
            ("/bin/bash -lc 'cd /w && git push origin a'", |p| {
                assert_eq!(p.dir, s("/w"));
                assert_eq!(p.local, s("a"));
            }),
            (
                "set -euo pipefail\ngit add -A\ngit commit -m x\ngit push origin a",
                |p| {
                    assert!(p.commits_first);
                    assert_eq!(p.local, s("a"));
                },
            ),
            (
                "cat > pr.md <<EOF\nbody\nEOF\ngh pr create --body-file pr.md",
                |p| assert_eq!(p.body, Body::Unreadable(WRITES_BODY.into())),
            ),
            (
                "cat <<-'EOF' > pr.md\n\tgit push origin nope\n\tEOF\ngit push origin a",
                |p| assert_eq!(p.local, s("a")),
            ),
            ("grep a <<< x && git push origin a", |p| {
                assert_eq!(p.local, s("a"))
            }),
        ];
        for (cmd, check) in rows {
            let p = parse(cmd).unwrap_or_else(|| panic!("no parse: {cmd}"));
            println!("{cmd}");
            check(&p);
        }
    }

    #[test]
    fn refuses_what_it_cannot_preview() {
        for cmd in [
            "git push --tags",
            "git push --all",
            "git push --mirror",
            "git push --delete origin x",
            "git push -d origin x",
            "git push origin :x",
            "git push origin a:",
            "git push origin a b",
            "git push origin $BRANCH",
            "git push $REMOTE a",
            "git push --repo=u",
            "git push --repo u",
            "git -C $REPO push",
            "cd $REPO && git push",
            "git --git-dir=x push",
            "git --work-tree x push",
            "git push origin a && git push origin b",
            "git push origin 'refs/heads/*'",
            "gh pr create --base $B",
        ] {
            unpreviewable(cmd);
        }
    }

    #[test]
    fn ignores_commands_that_do_not_push() {
        for cmd in [
            "echo git push",
            "git status",
            "gh pr view",
            "git stash push",
            "gh pr list",
            "git -C x status",
            "cat > n.txt <<'EOF'\ngit push origin nope\nEOF\ngit status",
            "",
            "FOO=1",
        ] {
            assert!(parse(cmd).is_none(), "should be None: {cmd}");
        }
    }

    #[test]
    fn reads_quotes_as_a_shell_does() {
        type Check = fn(&Parsed) -> bool;
        let rows: [(&str, Check); 11] = [
            (
                "gh pr create --title t --body \"$(cat <<'EOF'\n## Summary\n- plain\nEOF\n)\"",
                |p| p.pushes && matches!(p.body, Body::Unreadable(_)),
            ),
            (
                "git commit -m \"$(cat <<'EOF'\nfeat: x\n\nuses \"even\" quotes\nEOF\n)\" && git push -u origin a && gh pr create --title t --body \"$(cat <<'EOF'\n- plain\nEOF\n)\"",
                |p| p.commits_first && p.local == s("a") && matches!(p.body, Body::Unreadable(_)),
            ),
            (
                "git commit -m \"$(cat <<'EOF'\nfeat: x\n\nsupports a 5\" disk\nEOF\n)\" && git push -u origin a && gh pr create --title t --body-file pr.md",
                |p| p.commits_first && p.local == s("a") && p.body == Body::File("pr.md".into()),
            ),
            ("git commit -m \"say \\\"hi\" && git push origin a", |p| {
                p.commits_first && p.local == s("a")
            }),
            ("git commit -m 'it'\\''s' && git push origin a", |p| {
                p.commits_first && p.local == s("a")
            }),
            ("# don't push yet\ngit push origin a", |p| {
                p.local == s("a")
            }),
            ("git status # isn't this it\ngit push origin a", |p| {
                p.local == s("a")
            }),
            ("git commit -m \"`echo \"x`\" && git push origin a", |p| {
                p.commits_first && p.local == s("a")
            }),
            ("gh pr create --body \"\" --title t", |p| {
                p.body == Body::Inline(String::new())
            }),
            ("git commit -m \"a \\$(b)\" && git push origin a", |p| {
                p.local == s("a")
            }),
            ("git push origin $(git branch --show-current)", |p| {
                p.unpreviewable.is_some()
            }),
        ];
        for (cmd, check) in rows {
            let p = parse(cmd).unwrap_or_else(|| panic!("no parse: {cmd}"));
            assert!(check(&p), "{cmd}: {p:?}");
        }
    }

    #[test]
    fn unfinished_commands_do_not_panic() {
        let cmd = "git commit -m \"$(cat <<-'EOF' # c\n\tx\n\tEOF\n)\" && echo `a\\b` 'c' ${d} \\\n\
                   \"e\\\"f\" \\g && git push origin a # end";
        for (i, _) in cmd.char_indices() {
            let _ = parse(&cmd[..i]);
        }
        assert_eq!(parse(cmd).unwrap().local, s("a"));
    }

    #[test]
    fn a_pr_body_cannot_invent_a_push() {
        let p = parse(
            "gh pr create --title t --body \"$(cat <<'EOF'\n## Summary\nRun \"this\n\ngit push origin evil\nEOF\n)\"",
        )
        .unwrap();
        assert!(matches!(p.body, Body::Unreadable(_)), "{p:?}");
        assert_eq!((p.local, p.remote, p.unpreviewable), (None, None, None));
        assert!(
            parse("echo \"$(cat <<EOF\ngit push origin evil\nEOF\n)\"").is_none(),
            "a heredoc in a substitution isn't a command"
        );
    }

    #[test]
    fn reads_through_simple_prefixes() {
        for cmd in [
            "if true; then git push origin a; fi",
            "if git push origin a; then echo ok; fi",
            "for b in a; do git push origin a; done",
            "while true; do git push origin a; done",
            "if false; then :; else git push origin a; fi",
            "sudo git push origin a",
            "env git push origin a",
            "env A=1 B=2 git push origin a",
            "time git push origin a",
            "command git push origin a",
            "timeout 60 git push origin a",
            "timeout 5m git push origin a",
            "/usr/bin/env A=1 git push origin a",
            "{ git push origin a; }",
            "! git push origin a",
            "A=1 sudo B=2 git push origin a",
        ] {
            let p = parse(cmd).unwrap_or_else(|| panic!("no parse: {cmd}"));
            assert_eq!(p.local, s("a"), "{cmd}");
            assert_eq!(p.unpreviewable, None, "{cmd}");
        }
        unpreviewable("git push origin a; sudo git push origin b");
        assert!(parse("command -v git").is_none());
        assert!(parse("timeout").is_none());
    }

    #[test]
    fn body_folder_follows_the_shell() {
        type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>);
        let rows: [Row; 6] = [
            ("git -C /wt push origin a && gh pr create -F pr.md", s2("/wt"), None),
            (
                "cd /a && git push origin x && cd /b && gh pr edit -F pr.md",
                s2("/a"),
                s2("/b"),
            ),
            ("cd /a && git -C sub push && gh pr create -F pr.md", s2("/a/sub"), s2("/a")),
            ("gh pr edit -F pr.md && git -C /wt push origin a", s2("/wt"), None),
            ("cd /a && gh pr edit -F pr.md", s2("/a"), s2("/a")),
            ("git -C /wt status && gh pr create -F pr.md", None, None),
        ];
        for (cmd, dir, body_dir) in rows {
            let p = parse(cmd).unwrap();
            assert_eq!(p.dir.as_deref(), dir, "{cmd}");
            assert_eq!(p.body_dir.as_deref(), body_dir, "{cmd}");
        }
        let p = parse("git push origin a && cd \"$X\" && gh pr edit -F pr.md").unwrap();
        assert_eq!(p.unpreviewable, None);
        assert!(matches!(p.body, Body::Unreadable(_)), "{p:?}");
        let p = parse("git push origin a && cd \"$X\" && gh pr edit -F /abs/pr.md").unwrap();
        assert_eq!(p.body, Body::File("/abs/pr.md".into()));
        let p = parse("cd $A && cd /b && git push").unwrap();
        assert_eq!((p.dir.as_deref(), p.unpreviewable), (Some("/b"), None));
    }

    #[test]
    fn an_unknown_git_add_only_matters_to_a_commit() {
        assert_eq!(parse("git add $F && git push").unwrap().unpreviewable, None);
        let p = parse("git add $F && gh pr edit --body hi").unwrap();
        assert_eq!(p.unpreviewable, None);
        unpreviewable("git add $F && git commit -m x && git push");
        unpreviewable("git add $F && git commit -m x && gh pr create");
    }

    #[test]
    fn a_body_file_the_command_writes() {
        for cmd in [
            "cat > pr.md <<'EOF'\nfresh\nEOF\ngh pr edit --body-file pr.md",
            "cat <<'EOF' > pr.md\nfresh\nEOF\ngh pr edit --body-file pr.md",
            "echo fresh > pr.md && gh pr edit -F pr.md",
            "echo fresh >pr.md && gh pr edit -F pr.md",
            "echo fresh>pr.md && gh pr edit -F pr.md",
            "echo fresh >> pr.md && gh pr edit -F pr.md",
            "echo fresh 2> pr.md && gh pr edit -F pr.md",
            "echo fresh > ./pr.md && gh pr edit -F pr.md",
            "echo fresh > pr.md && gh pr edit -F ./pr.md",
            "echo fresh | tee pr.md && gh pr edit -F pr.md",
            "echo fresh | tee -a pr.md && gh pr edit -F pr.md",
            "echo fresh | /usr/bin/tee a.md pr.md && gh pr edit -F pr.md",
            "cd /a && echo fresh > pr.md && gh pr edit -F pr.md",
            "echo fresh > /a/pr.md && cd /a && gh pr edit -F pr.md",
            "bash -c 'echo fresh > pr.md' && gh pr edit -F pr.md",
        ] {
            let p = parse(cmd).unwrap();
            assert_eq!(p.body, Body::Unreadable(WRITES_BODY.into()), "{cmd}");
        }
        for cmd in [
            "echo fresh > other.md && gh pr edit -F pr.md",
            "cat pr.md > out.md && gh pr edit -F pr.md",
            "cat < pr.md && gh pr edit -F pr.md",
            "cd /a && echo fresh > pr.md && cd /b && gh pr edit -F pr.md",
            "gh pr edit -F pr.md && echo done > pr.md",
            "gh pr edit --title '>' -F pr.md",
            "git commit -m 'see > pr.md' && gh pr edit -F pr.md",
        ] {
            let p = parse(cmd).unwrap();
            assert_eq!(p.body, Body::File("pr.md".into()), "{cmd}");
        }
    }

    #[test]
    fn commands_before_a_push_do_not_leak() {
        let p = parse("git -C other status && git push origin a").unwrap();
        assert_eq!(p.dir, None);
        let p = parse("git push origin a && cd /late").unwrap();
        assert_eq!(p.dir, None);
    }

    struct Fixture {
        dir: std::path::PathBuf,
        work: std::path::PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir =
                std::env::temp_dir().join(format!("drovr-push-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            let f = Fixture {
                work: dir.join("work"),
                dir,
            };
            let run = |cmd: &str| f.run_in(&f.dir, cmd);
            run("git init -q --bare -b main origin.git");
            run("git clone -q origin.git work 2>/dev/null");
            f.sh("git config user.name t && git config user.email t@example.com");
            f.sh("git checkout -q -B main");
            f.commit("base.txt", "base\n", "chore: base");
            f.sh("git push -q -u origin main");
            f
        }

        fn run_in(&self, dir: &Path, cmd: &str) {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{cmd}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        fn sh(&self, cmd: &str) {
            self.run_in(&self.work, cmd);
        }

        fn commit(&self, file: &str, text: &str, msg: &str) {
            std::fs::write(self.work.join(file), text).unwrap();
            self.sh(&format!("git add {file} && git commit -q -m '{msg}'"));
        }

        fn root(&self) -> String {
            self.work.display().to_string()
        }

        fn preview(&self, cmd: &str) -> Preview {
            preview(cmd, &self.root(), true)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn unavailable(p: &Preview) -> &str {
        p.unavailable
            .as_deref()
            .unwrap_or_else(|| panic!("should be unavailable: {p:?}"))
    }

    #[test]
    fn unpushed_commits_only() {
        let f = Fixture::new("unpushed");
        f.sh("git checkout -q -b fix/x && git push -q -u origin fix/x");
        f.commit("new.txt", "fresh\n", "fix: new");
        let p = f.preview("git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(p.pushes);
        assert_eq!(p.commits.len(), 1);
        assert!(p.commits[0].ends_with(" fix: new"));
        assert!(p.this_push.contains("+fresh"));
        assert!(!p.this_push.contains("base"));
        assert_eq!(
            p.summary_line,
            "work \u{b7} fix/x \u{b7} 1 commit, 1 file, +1 \u{2212}0"
        );
        assert!(p.tracking.is_some());
        assert_eq!((p.branch.as_str(), p.remote.as_str()), ("fix/x", "origin"));
        assert_eq!(p.root, f.root());
    }

    #[test]
    fn new_branch() {
        let f = Fixture::new("newbranch");
        f.sh("git checkout -q -b feat/y");
        f.commit("a.txt", "a\n", "feat: a");
        f.commit("b.txt", "b\n", "feat: b");
        let p = f.preview("git push -u origin feat/y");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!(p.tracking, None);
        assert_eq!(p.commits.len(), 2);
        assert!(p.summary_line.contains("2 commits, 2 files"));
    }

    #[test]
    fn explicit_git_c() {
        let f = Fixture::new("gitc");
        let elsewhere = f.dir.display().to_string();
        let p = preview(&format!("git -C {} push", f.root()), &elsewhere, false);
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!(p.root, f.root());
        assert!(!p.assumed_folder);
    }

    #[test]
    fn assumed_folder() {
        let f = Fixture::new("assumed");
        assert!(preview("git push", &f.root(), false).assumed_folder);
        assert!(!preview("git push", &f.root(), true).assumed_folder);
    }

    #[test]
    fn refspec() {
        let f = Fixture::new("refspec");
        f.sh("git checkout -q -b a");
        f.commit("a.txt", "a\n", "feat: a");
        f.sh("git push -q origin a:b");
        f.commit("a2.txt", "a2\n", "feat: a2");
        let p = f.preview("git push origin a:b");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!((p.branch.as_str(), p.remote_branch.as_str()), ("a", "b"));
        assert!(p.tracking.is_some());
        assert_eq!(p.commits.len(), 1);
    }

    #[test]
    fn nothing_to_push() {
        let f = Fixture::new("nothing");
        let p = f.preview("git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(p.this_push.is_empty());
        assert!(p.summary_line.contains("nothing to push"));
    }

    fn amended(name: &str) -> Fixture {
        let f = Fixture::new(name);
        f.sh("git checkout -q -b f");
        f.commit("f.txt", "one\n", "feat: f");
        f.sh("git push -q -u origin f");
        std::fs::write(f.work.join("f.txt"), "two\n").unwrap();
        f.sh("git commit -q -a --amend -m 'feat: f2'");
        f
    }

    #[test]
    fn force_after_amend() {
        let f = amended("force");
        let p = f.preview("git push --force-with-lease");
        assert_eq!(
            p.notices,
            ["History is being rewritten; this diff may include changes from main."]
        );
        assert!(p.this_push.contains("+two"), "{}", p.this_push);
    }

    #[test]
    fn no_force_diverged() {
        let f = amended("noforce");
        let p = f.preview("git push");
        assert_eq!(p.notices.len(), 1);
        assert!(
            p.notices[0].contains("isn't an ancestor"),
            "{:?}",
            p.notices
        );
    }

    #[test]
    fn tag_target() {
        let f = Fixture::new("tag");
        f.sh("git tag v1");
        assert!(unavailable(&f.preview("git push origin v1")).contains("v1"));
    }

    #[test]
    fn parsed_unpreviewable() {
        let f = Fixture::new("tags");
        let reason = parse("git push --tags").unwrap().unpreviewable;
        assert_eq!(f.preview("git push --tags").unavailable, reason);
        assert!(reason.is_some());
    }

    #[test]
    fn missing_dir() {
        let f = Fixture::new("missing");
        f.sh("git worktree add -q wt -b w && rm -rf wt");
        let p = f.preview("git -C wt push");
        assert!(unavailable(&p).contains("doesn't exist"), "{p:?}");
        assert!(p.root.is_empty());
    }

    #[test]
    fn not_a_repo() {
        let f = Fixture::new("notrepo");
        let plain = f.dir.join("plain");
        std::fs::create_dir(&plain).unwrap();
        let p = preview("git push", &plain.display().to_string(), true);
        assert!(unavailable(&p).contains("git repository"));
    }

    #[test]
    fn empty_cwd_cannot_preview() {
        for cmd in ["git push", "git push origin main", "gh pr create --title t"] {
            let p = preview(cmd, "", false);
            assert!(unavailable(&p).contains("doesn't exist"), "{cmd}: {p:?}");
        }
    }

    #[test]
    fn detached_head() {
        let f = Fixture::new("detached");
        f.sh("git checkout -q --detach");
        assert!(unavailable(&f.preview("git push")).contains("HEAD"));
    }

    #[test]
    fn no_remote() {
        let f = Fixture::new("noremote");
        f.sh("git remote remove origin");
        assert!(unavailable(&f.preview("git push")).contains("isn't a remote"));
    }

    #[test]
    fn option_like_refs_are_refused() {
        let f = Fixture::new("dashref");
        assert!(unavailable(&f.preview("git push origin main:-x")).contains("-x"));
    }

    #[test]
    fn commands_that_do_not_push() {
        let f = Fixture::new("nopush");
        let p = f.preview("git stash push");
        assert_eq!(
            p.unavailable.as_deref(),
            Some("drovr couldn't find a push it can preview in this command. That doesn't mean it won't push.")
        );
        let p = f.preview("gh pr edit 7 --body-file x");
        assert!(p.unavailable.is_none() && !p.pushes);
        assert_eq!(p.root, f.root());
    }

    #[test]
    fn config_safety() {
        let f = Fixture::new("safety");
        let marker = f.dir.join("ran");
        let script = f.dir.join("ext.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        f.sh(&format!(
            "chmod +x {0} && git config diff.external {0}",
            script.display()
        ));
        f.sh("git checkout -q -b s && git push -q -u origin s");
        f.commit("base.txt", "changed\n", "fix: change");
        let p = f.preview("git push");
        assert!(p.this_push.contains("+changed"), "{p:?}");
        assert!(!marker.exists());
    }

    fn commit_push(f: &Fixture, edit: bool) {
        f.sh("git checkout -q -b fix/x && git push -q -u origin fix/x");
        f.commit("a.txt", "a\n", "feat: a");
        f.commit("b.txt", "b\n", "feat: b");
        f.sh("git push -q origin fix/x");
        if edit {
            std::fs::write(f.work.join("a.txt"), "a2\n").unwrap();
            std::fs::write(f.work.join("b.txt"), "b2\n").unwrap();
            f.sh("git add a.txt");
        }
    }

    #[test]
    fn uncommitted_tracked() {
        let f = Fixture::new("unc-tracked");
        commit_push(&f, true);
        let p = f.preview("git commit -am x && git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        let u = p.uncommitted.as_deref().unwrap_or_else(|| panic!("{p:?}"));
        assert!(u.contains("+a2") && u.contains("+b2"), "{u}");
        assert_eq!(p.uncommitted_note, None);
        assert!(
            p.summary_line
                .ends_with("; plus uncommitted changes in 2 files"),
            "{}",
            p.summary_line
        );
    }

    #[test]
    fn uncommitted_one_file() {
        let f = Fixture::new("unc-one");
        commit_push(&f, false);
        std::fs::write(f.work.join("a.txt"), "a2\n").unwrap();
        let p = f.preview("git commit -am x && git push");
        assert!(
            p.summary_line
                .ends_with("; plus uncommitted changes in 1 file"),
            "{}",
            p.summary_line
        );
    }

    #[test]
    fn add_all_untracked() {
        let f = Fixture::new("unc-all");
        commit_push(&f, false);
        std::fs::write(f.work.join("new.txt"), "fresh\n").unwrap();
        let p = f.preview("git add -A && git commit -m x && git push");
        let u = p.uncommitted.as_deref().unwrap_or_else(|| panic!("{p:?}"));
        assert!(u.contains("b/new.txt") && u.contains("+fresh"), "{u}");
        assert!(u.contains("--- /dev/null"));
        assert!(p.summary_line.ends_with("in 1 file"), "{}", p.summary_line);
    }

    #[test]
    fn add_paths() {
        let f = Fixture::new("unc-paths");
        commit_push(&f, false);
        std::fs::write(f.work.join("a.txt"), "a2\n").unwrap();
        std::fs::write(f.work.join("b.txt"), "b2\n").unwrap();
        std::fs::write(f.work.join("a-new.txt"), "n\n").unwrap();
        std::fs::write(f.work.join("c-new.txt"), "n\n").unwrap();
        let p = f.preview("git add a.txt a-new.txt && git commit -m x && git push");
        let u = p.uncommitted.as_deref().unwrap_or_else(|| panic!("{p:?}"));
        assert!(u.contains("+a2") && u.contains("a-new.txt"), "{u}");
        assert!(!u.contains("b2") && !u.contains("c-new.txt"), "{u}");
    }

    #[test]
    fn no_add_untracked_excluded() {
        let f = Fixture::new("unc-noadd");
        commit_push(&f, true);
        std::fs::write(f.work.join("new.txt"), "fresh\n").unwrap();
        let p = f.preview("git commit -am x && git push");
        let u = p.uncommitted.as_deref().unwrap_or_else(|| panic!("{p:?}"));
        assert!(!u.contains("new.txt"), "{u}");
    }

    #[test]
    fn nothing_uncommitted() {
        let f = Fixture::new("unc-none");
        commit_push(&f, false);
        let p = f.preview("git commit -am x && git push");
        assert_eq!((p.uncommitted, p.uncommitted_note), (None, None));
        assert!(!p.summary_line.contains("uncommitted"));
    }

    #[test]
    fn no_commit_no_section() {
        let f = Fixture::new("unc-nocommit");
        commit_push(&f, true);
        let p = f.preview("git push");
        assert_eq!((p.uncommitted, p.uncommitted_note), (None, None));
    }

    #[test]
    fn filter_refused() {
        let f = Fixture::new("unc-filter");
        let sentinel = f.dir.join("ran");
        f.sh(&format!(
            "git config filter.x.clean 'touch {}; cat'",
            sentinel.display()
        ));
        std::fs::write(f.work.join(".gitattributes"), "*.txt filter=x\n").unwrap();
        f.sh("git add .gitattributes && git commit -q -m attrs");
        commit_push(&f, true);
        f.commit("c.rs", "c\n", "feat: c");
        let _ = std::fs::remove_file(&sentinel);
        let p = f.preview("git commit -am x && git push");
        assert_eq!(p.uncommitted, None);
        assert!(
            p.uncommitted_note
                .as_deref()
                .is_some_and(|n| n.contains("content filters")),
            "{p:?}"
        );
        assert!(!sentinel.exists());
        assert_eq!(p.commits.len(), 1, "{p:?}");
        assert!(p.this_push.contains("+c"));
        assert!(!p.summary_line.contains("uncommitted"));
    }

    #[test]
    fn global_filter_allowed() {
        let f = Fixture::new("unc-global");
        let global = f.dir.join("gitconfig");
        std::fs::write(&global, "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n").unwrap();
        let run = |env: &str| {
            shell::sh(
                &format!("{env} {FILTER_CHECK}"),
                Some(&f.work),
                None,
                Duration::from_secs(10),
            )
            .unwrap()
        };
        let o = run(&format!("GIT_CONFIG_GLOBAL={}", global.display()));
        assert_eq!((o.code, o.stdout.trim()), (1, ""), "{}", o.stderr);
        assert!(!repo_has_filter(&f.root()));
        f.sh("git config filter.lfs.clean 'git-lfs clean -- %f'");
        let o = run(&format!("GIT_CONFIG_GLOBAL={}", global.display()));
        assert_eq!(o.code, 0);
        assert!(repo_has_filter(&f.root()));
    }

    #[test]
    fn linked_worktree_without_filter() {
        let f = Fixture::new("unc-wt");
        commit_push(&f, false);
        f.sh("git worktree add -q ../wt -b other");
        let wt = f.dir.join("wt");
        std::fs::write(wt.join("a.txt"), "wt edit\n").unwrap();
        let p = preview(
            "git commit -am x && git push",
            &wt.display().to_string(),
            true,
        );
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!(p.uncommitted_note, None, "{p:?}");
        assert!(
            p.uncommitted.as_deref().unwrap_or("").contains("+wt edit"),
            "{p:?}"
        );
    }

    #[test]
    fn worktree_config_filter_refused() {
        let f = Fixture::new("unc-wtcfg");
        commit_push(&f, true);
        f.sh("git config extensions.worktreeConfig true");
        f.sh("git config --worktree filter.x.clean cat");
        let p = f.preview("git commit -am x && git push");
        assert_eq!(p.uncommitted, None);
        assert!(p.uncommitted_note.is_some(), "{p:?}");
    }

    #[test]
    fn whole_branch_with_uncommitted() {
        let f = Fixture::new("unc-whole");
        commit_push(&f, true);
        let p = f.preview("git commit -am x && git push");
        let w = p.whole_branch.as_ref().unwrap_or_else(|| panic!("{p:?}"));
        assert!(!w.text.contains("a2") && !w.text.contains("b2"));
        assert!(w.marked.is_empty());
        assert!(p.uncommitted.is_some());
    }

    #[test]
    fn not_on_named_branch() {
        let f = Fixture::new("unc-other");
        commit_push(&f, true);
        f.sh("git branch other");
        let p = f.preview("git commit -am x && git push origin other");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!((p.uncommitted, p.uncommitted_note), (None, None));
    }

    #[test]
    fn reads_shortstat() {
        for (text, want) in [
            (
                " 2 files changed, 14 insertions(+), 3 deletions(-)\n",
                (2, 14, 3),
            ),
            (" 1 file changed, 1 insertion(+)\n", (1, 1, 0)),
            (" 1 file changed, 2 deletions(-)\n", (1, 0, 2)),
            ("", (0, 0, 0)),
        ] {
            assert_eq!(shortstat(text), want, "{text}");
        }
    }

    fn added(diff: &str) -> Vec<(String, Vec<u32>)> {
        let mut v: Vec<(String, Vec<u32>)> = added_lines(diff)
            .into_iter()
            .map(|(k, set)| {
                let mut n: Vec<u32> = set.into_iter().collect();
                n.sort();
                (k, n)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn added_lines_reads_hunks() {
        let diff = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n\
@@ -1,3 +1,4 @@\n one\n+two\n three\n four\n@@ -10,2 +11,3 @@\n ten\n+eleven\n+twelve\n\
diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n\
@@ -1 +0,0 @@\n-bye\n\
diff --git a/my file.txt b/my file.txt\n--- a/my file.txt\t\n+++ b/my file.txt\t\n\
@@ -0,0 +1 @@\n+hi\n\\ No newline at end of file\n";
        assert_eq!(
            added(diff),
            vec![
                ("a.txt".to_string(), vec![2, 12, 13]),
                ("my file.txt".to_string(), vec![1]),
            ]
        );
        assert!(added("").is_empty());
    }

    #[test]
    fn content_like_headers() {
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n keep\n--- gone\n\
+++ not a header\n end\ndiff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1 +1,2 @@\n x\n+y\n";
        assert_eq!(
            added(diff),
            vec![("f".to_string(), vec![2]), ("g".to_string(), vec![2])]
        );
    }

    #[test]
    fn mark_picks_lines_by_number() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,3 @@\n\
+one\n+two\n+three\n";
        let mut want = HashMap::new();
        want.insert("a.txt".to_string(), HashSet::from([2u32]));
        assert_eq!(mark(diff, &want), vec![5]);
        want.insert("other".to_string(), HashSet::from([1u32]));
        assert_eq!(mark(diff, &want), vec![5]);
        assert!(mark(diff, &HashMap::new()).is_empty());
    }

    fn marked_text(p: &Preview) -> Vec<String> {
        let w = p.whole_branch.as_ref().unwrap_or_else(|| panic!("{p:?}"));
        let lines: Vec<&str> = w.text.lines().collect();
        w.marked
            .iter()
            .map(|i| lines[*i as usize].to_string())
            .collect()
    }

    fn pushed_branch(name: &str) -> Fixture {
        let f = Fixture::new(name);
        f.sh("git checkout -q -b fix/x");
        f
    }

    #[test]
    fn marks_by_line_number() {
        let f = pushed_branch("marks");
        f.commit("a.rs", "fn a() {\n    one();\n    two();\n}\n", "feat: fn");
        f.sh("git push -q -u origin fix/x");
        f.commit(
            "a.rs",
            "fn a() {\n    one();\n    n1();\n    n2();\n    n3();\n    two();\n}\n",
            "feat: more",
        );
        let p = f.preview("git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert_eq!(marked_text(&p), ["+    n1();", "+    n2();", "+    n3();"]);
        let w = p.whole_branch.unwrap();
        assert_eq!((w.commits, w.files), (2, 1));
        assert!(w.text.contains("+fn a() {"));
    }

    #[test]
    fn shifted_lines() {
        let f = pushed_branch("shifted");
        f.commit("a.txt", "a\nb\nc\n", "feat: abc");
        f.sh("git push -q -u origin fix/x");
        f.commit("a.txt", "x\ny\na\nb\nc\n", "feat: xy");
        assert_eq!(marked_text(&f.preview("git push")), ["+x", "+y"]);
    }

    #[test]
    fn renamed_file() {
        let f = pushed_branch("renamed");
        f.commit("f.txt", "1\n2\n3\n4\n5\n", "feat: f");
        f.sh("git push -q -u origin fix/x");
        f.sh("git mv f.txt g.txt && printf '6\\n' >> g.txt && git add -A && git commit -q -m 'refactor: g'");
        assert_eq!(marked_text(&f.preview("git push")), ["+6"]);
    }

    #[test]
    fn binary_entry() {
        let f = pushed_branch("binary");
        let w = |n: &str, b: &[u8]| std::fs::write(f.work.join(n), b).unwrap();
        w("a.txt", b"a1\n");
        w("b.bin", b"\0\x01\x02");
        w("c.txt", b"c1\n");
        f.sh("git add -A && git commit -q -m 'feat: three'");
        f.sh("git push -q -u origin fix/x");
        w("a.txt", b"a1\na2\n");
        w("b.bin", b"\0\x01\x02\x03");
        w("c.txt", b"c1\nc2\n");
        f.sh("git add -A && git commit -q -m 'feat: more'");
        let p = f.preview("git push");
        assert!(p
            .whole_branch
            .as_ref()
            .unwrap()
            .text
            .contains("Binary files"));
        assert_eq!(marked_text(&p), ["+a2", "+c2"]);
    }

    #[test]
    fn odd_paths() {
        let f = pushed_branch("odd");
        let w = |n: &str, t: &str| std::fs::write(f.work.join(n), t).unwrap();
        w("my file.txt", "a\nb\nc\n");
        w("caf\u{e9}.txt", "a\nb\n");
        f.sh("git add -A && git commit -q -m 'feat: files'");
        f.sh("git push -q -u origin fix/x");
        w("my file.txt", "a\nb\nc\nd\n");
        w("caf\u{e9}.txt", "a\nb\nd\n");
        f.sh("git add -A && git commit -q -m 'feat: d'");
        assert_eq!(marked_text(&f.preview("git push")), ["+d", "+d"]);
    }

    #[test]
    fn whole_branch_uses_default() {
        let f = pushed_branch("default");
        f.commit("a.txt", "a\n", "feat: a");
        f.sh("git push -q -u origin fix/x");
        f.commit("b.txt", "b\n", "feat: b");
        let p = f.preview("git push");
        let w = p.whole_branch.unwrap();
        assert_eq!(w.base, "origin/main");
        assert!(w.text.contains("+a") && w.text.contains("+b"));
        assert!(!w.text.contains("base.txt"));
    }

    fn with_dev(name: &str) -> Fixture {
        let f = Fixture::new(name);
        f.sh("git checkout -q -b dev");
        f.commit("d.txt", "d\n", "feat: dev");
        f.sh("git push -q origin dev");
        f.sh("git checkout -q -b fix/x");
        f.commit("a.txt", "a\n", "feat: a");
        f.sh("git push -q -u origin fix/x");
        f.commit("b.txt", "b\n", "feat: b");
        f
    }

    #[test]
    fn whole_branch_base() {
        let f = with_dev("dev");
        let p = f.preview("git push && gh pr create --base dev");
        let w = p.whole_branch.clone().unwrap_or_else(|| panic!("{p:?}"));
        assert_eq!(w.base, "origin/dev");
        assert_eq!(w.commits, 2);
        assert!(!w.text.contains("d.txt"));
        assert!(p.notices.is_empty(), "{p:?}");
    }

    #[test]
    fn whole_branch_unknown_base() {
        let f = with_dev("nobase");
        let p = f.preview("git push && gh pr create --base nope");
        let w = p.whole_branch.clone().unwrap_or_else(|| panic!("{p:?}"));
        assert_eq!(w.base, "origin/main");
        assert!(p.notices.iter().any(|n| n.contains("nope")), "{p:?}");
    }

    #[test]
    fn whole_branch_hidden() {
        let f = pushed_branch("hidden");
        f.commit("a.txt", "a\n", "feat: a");
        let p = f.preview("git push -u origin fix/x");
        assert!(p.unavailable.is_none() && p.tracking.is_none(), "{p:?}");
        assert!(p.whole_branch.is_none());
    }

    #[test]
    fn whole_branch_not_ahead() {
        let f = pushed_branch("notahead");
        f.sh("git push -q -u origin fix/x");
        assert!(f.preview("git push").whole_branch.is_none());
    }

    #[test]
    fn whole_branch_of_another_branch() {
        let f = Fixture::new("other");
        f.sh("git checkout -q -b x");
        f.commit("x.txt", "x\n", "feat: x");
        f.sh("git checkout -q main");
        f.commit("m.txt", "m\n", "feat: m");
        f.sh("git push -q origin main");
        f.sh("git checkout -q -b other");
        f.commit("o1.txt", "o1\n", "feat: o1");
        f.sh("git push -q -u origin other");
        f.commit("o2.txt", "o2\n", "feat: o2");
        f.sh("git checkout -q x");
        let p = f.preview("git push origin other");
        let w = p.whole_branch.clone().unwrap_or_else(|| panic!("{p:?}"));
        assert_eq!(w.commits, 2);
        assert!(w.text.contains("+o1") && w.text.contains("+o2"));
        assert!(!w.text.contains("m.txt") && !w.text.contains("x.txt"));
        assert_eq!(marked_text(&p), ["+o2"]);
    }

    fn fake_gh(f: &Fixture, script: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = f.dir.join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn summary_file_read() {
        let f = Fixture::new("sum-file");
        f.commit("a.txt", "a\n", "feat: a");
        std::fs::write(f.work.join("pr.md"), "# Title\nbody").unwrap();
        let p = f.preview("git push && gh pr create --body-file pr.md");
        let s = p.pr_summary.unwrap();
        assert_eq!(s.text.as_deref(), Some("# Title\nbody"));
        assert_eq!(s.note, None);
    }

    #[test]
    fn summary_only() {
        let f = Fixture::new("sum-only");
        std::fs::write(f.work.join("pr.md"), "x").unwrap();
        let p = f.preview("gh pr edit --body-file pr.md");
        assert!(!p.pushes && p.unavailable.is_none());
        assert_eq!(p.pr_summary.unwrap().text.as_deref(), Some("x"));
    }

    #[test]
    fn summary_inline() {
        let f = Fixture::new("sum-inline");
        let p = f.preview("gh pr edit --body 'hello'");
        assert_eq!(p.pr_summary.unwrap().text.as_deref(), Some("hello"));
    }

    #[test]
    fn summary_none_when_no_body_or_unavailable() {
        let f = Fixture::new("sum-none");
        assert!(f.preview("gh pr edit --title t").pr_summary.is_none());
        let p = f.preview("git push nosuchremote && gh pr create --body hi");
        assert!(p.unavailable.is_some() && p.pr_summary.is_none());
    }

    #[test]
    fn summary_notes() {
        let f = Fixture::new("sum-notes");
        let w = &f.work;
        std::fs::write(w.join("big.md"), vec![b'a'; MAX_SUMMARY as usize + 1]).unwrap();
        std::fs::write(w.join("max.md"), vec![b'a'; MAX_SUMMARY as usize]).unwrap();
        std::fs::create_dir(w.join("adir")).unwrap();
        f.sh("mkfifo fifo");
        for (cmd, note) in [
            ("gh pr edit -F nope.md", "doesn't exist yet"),
            ("gh pr edit -F big.md", "too large"),
            ("gh pr edit -F adir", "isn't a regular file"),
            ("gh pr edit -F fifo", "isn't a regular file"),
            ("gh pr edit -F -", "standard input"),
        ] {
            let s = f.preview(cmd).pr_summary.unwrap();
            assert_eq!(s.text, None, "{cmd}");
            assert!(s.note.unwrap().contains(note), "{cmd}");
        }
        let s = f.preview("gh pr edit -F max.md").pr_summary.unwrap();
        assert_eq!(s.text.map(|t| t.len()), Some(MAX_SUMMARY as usize));
    }

    #[test]
    fn summary_unreadable() {
        let s = pr_summary(&Body::Unreadable("why".into()), Path::new("/")).unwrap();
        assert_eq!((s.text, s.note.as_deref()), (None, Some("why")));
    }

    #[test]
    fn github_tip_check() {
        let f = Fixture::new("moved");
        let root = f.root();
        let cases: [(&str, Option<&str>, Option<bool>); 7] = [
            ("echo abc", Some("abc"), Some(false)),
            ("echo abc", Some("def"), Some(true)),
            ("echo abc", None, Some(true)),
            (
                "echo 'gh: Not Found (HTTP 404)' >&2; exit 1",
                None,
                Some(false),
            ),
            ("echo 'HTTP 404' >&2; exit 1", Some("abc"), Some(true)),
            ("echo auth >&2; exit 1", Some("abc"), None),
            ("exit 0", Some("abc"), None),
        ];
        for (script, tracking, want) in cases {
            let gh = fake_gh(&f, script);
            assert_eq!(github_moved(&gh, &root, "b", tracking), want, "{script}");
        }
        let missing = f.dir.join("nope").display().to_string();
        assert_eq!(github_moved(&missing, &root, "b", Some("abc")), None);
    }

    #[test]
    fn branch_is_quoted() {
        let f = Fixture::new("quoted");
        let out = f.dir.join("args");
        let gh = fake_gh(&f, &format!("echo \"$#|$2\" > {}; echo abc", out.display()));
        github_moved(&gh, &f.root(), "a b", Some("abc"));
        assert_eq!(
            std::fs::read_to_string(out).unwrap().trim(),
            "4|repos/{owner}/{repo}/git/ref/heads/a b"
        );
    }

    #[test]
    fn invalid_utf8_keeps_the_diff() {
        let f = Fixture::new("latin1");
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.commit("ok.txt", "visible\n", "feat: ok");
        std::fs::write(f.work.join("latin1.txt"), b"caf\xe9 secret\n").unwrap();
        f.sh("git add -A && git commit -q -m 'feat: latin1'");
        let p = f.preview("git push");
        assert!(p.this_push.contains("+visible"), "{p:?}");
        assert!(p.this_push.contains("+caf\u{fffd} secret"), "{p:?}");
        assert_eq!(p.this_push_note, None);
        let w = p.whole_branch.unwrap();
        assert!(w.text.contains("+visible"));
    }

    #[test]
    fn empty_push_notes() {
        let unreadable = Some(DIFF_MISSING);
        for (commits, files, text, want) in [
            (0, 0, "", Some("Nothing new to push.")),
            (1, 0, "", Some("These commits have no file changes.")),
            (2, 0, "\n", Some("These commits have no file changes.")),
            (2, 2, "", unreadable),
            (0, 1, "", unreadable),
            (2, 2, "diff", None),
        ] {
            assert_eq!(
                this_push_note(commits, files, text),
                want,
                "{commits} {files} {text:?}"
            );
        }
    }

    #[test]
    fn empty_commit_says_so() {
        let f = Fixture::new("emptycommit");
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.sh("git commit -q --allow-empty -m 'chore: empty'");
        let p = f.preview("git push");
        assert_eq!(
            p.this_push_note.as_deref(),
            Some("These commits have no file changes.")
        );
        assert!(p.summary_line.contains("1 commit, 0 files"), "{p:?}");
    }

    #[test]
    fn nothing_new_says_so() {
        let f = Fixture::new("nothingnew");
        let p = f.preview("git push");
        assert_eq!(p.this_push_note.as_deref(), Some("Nothing new to push."));
    }

    /// A program that leaves a marker if anything runs it.
    fn tripwire(f: &Fixture) -> (String, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let marker = f.dir.join("ran");
        let script = f.dir.join("tripwire.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script.display().to_string(), marker)
    }

    #[test]
    fn signature_check_runs_nothing() {
        let f = Fixture::new("c2-sig");
        let (script, marker) = tripwire(&f);
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.commit("a.txt", "a\n", "feat: a");
        f.sh(
            "c=$(git cat-file commit HEAD | awk '/^committer/{print; \
             print \"gpgsig -----BEGIN PGP SIGNATURE-----\"; print \" abc\"; \
             print \" -----END PGP SIGNATURE-----\"; next} {print}' \
             | git hash-object -t commit -w --stdin) && git update-ref refs/heads/x \"$c\"",
        );
        f.sh(&format!(
            "git config gpg.program {script} && git config log.showSignature true"
        ));
        let p = f.preview("git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(!marker.exists(), "gpg.program ran");
        assert_eq!(p.commits.len(), 1, "{p:?}");
    }

    #[test]
    fn index_refresh_runs_no_hook_and_writes_nothing() {
        let f = Fixture::new("c2-hook");
        let (script, marker) = tripwire(&f);
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.commit("a.txt", "a\n", "feat: a");
        f.commit("b.txt", "b\n", "feat: b");
        std::fs::copy(&script, f.work.join(".git/hooks/post-index-change")).unwrap();
        f.sh("sleep 1; touch a.txt b.txt");
        let index = std::fs::read(f.work.join(".git/index")).unwrap();
        let p = f.preview("git commit -am x && git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(!marker.exists(), "post-index-change ran");
        assert_eq!(std::fs::read(f.work.join(".git/index")).unwrap(), index);
    }

    #[test]
    fn dirty_submodule_filter_does_not_run() {
        let f = Fixture::new("c2-subfilter");
        let (script, marker) = tripwire(&f);
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.sh(
            "git init -q sub && cd sub && git config user.name t \
             && git config user.email t@example.com && echo a > a.txt \
             && echo '*.txt filter=x' > .gitattributes && git add -A && git commit -q -m s",
        );
        f.sh("git add sub 2>/dev/null; git commit -q -m 'chore: gitlink'");
        f.sh(&format!(
            "cd sub && git config filter.x.clean {script}; sleep 1; echo b > a.txt"
        ));
        let _ = std::fs::remove_file(&marker);
        assert!(!repo_has_filter(&f.root()));
        let p = f.preview("git commit -am x && git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(!marker.exists(), "the submodule's clean filter ran");
    }

    #[test]
    fn submodule_bump_runs_nothing_and_still_shows() {
        let f = Fixture::new("c2-subdiff");
        let (script, marker) = tripwire(&f);
        f.sh(
            "git init -q sub && cd sub && git config user.name t \
             && git config user.email t@example.com && echo a > a.txt \
             && git add -A && git commit -q -m s",
        );
        f.sh(
            "git add sub 2>/dev/null; git commit -q -m 'chore: gitlink'; \
             git checkout -q -b x; git push -q -u origin x",
        );
        f.sh("cd sub && echo b > a.txt && git add -A && git commit -q -m s2");
        f.sh("git add sub 2>/dev/null; git commit -q -m 'chore: bump'");
        f.sh(&format!(
            "git config diff.submodule diff; cd sub && git config diff.external {script}"
        ));
        let p = f.preview("git push");
        assert!(p.unavailable.is_none(), "{p:?}");
        assert!(!marker.exists(), "the submodule's diff.external ran");
        assert!(p.this_push.contains("+Subproject commit"), "{p:?}");
    }

    #[test]
    fn missing_blob_is_not_fetched() {
        let f = Fixture::new("c2-lazy");
        let (script, marker) = tripwire(&f);
        f.sh("git checkout -q -b x && git push -q -u origin x");
        f.commit("a.txt", "missing blob\n", "feat: a");
        f.sh("h=$(git rev-parse HEAD:a.txt); rm -f .git/objects/$(echo $h | cut -c1-2)/$(echo $h | cut -c3-)");
        f.sh(&format!(
            "git config core.repositoryformatversion 1 && git config extensions.partialClone origin \
             && git config remote.origin.promisor true && git config remote.origin.uploadpack {script}"
        ));
        let p = f.preview("git push");
        assert!(!marker.exists(), "a lazy fetch ran remote.origin.uploadpack");
        assert!(p.unavailable.is_some(), "{p:?}");
    }

    #[test]
    fn repo_colour_settings_do_not_reach_the_text() {
        for setting in ["color.ui", "color.diff"] {
            let f = Fixture::new(&format!("c2-{setting}"));
            f.sh("git checkout -q -b x");
            f.commit("a.txt", "a\n", "feat: a");
            f.sh("git push -q -u origin x");
            f.commit("a.txt", "a\nb\n", "feat: b");
            f.sh(&format!("git config {setting} always"));
            let p = f.preview("git push");
            assert!(!p.this_push.contains('\u{1b}'), "{setting}: {p:?}");
            assert_eq!(marked_text(&p), ["+b"], "{setting}");
        }
    }

    fn summary_text(p: &Preview) -> Option<&str> {
        p.pr_summary.as_ref().and_then(|s| s.text.as_deref())
    }

    #[test]
    fn body_file_is_read_from_the_shells_folder() {
        let f = Fixture::new("i1");
        let other = f.dir.join("other");
        std::fs::create_dir(&other).unwrap();
        let other_s = other.display().to_string();
        std::fs::write(other.join("pr.md"), "from the shell's folder").unwrap();
        std::fs::write(f.work.join("pr.md"), "from the worktree").unwrap();
        let cmd = format!(
            "git -C {} push origin main && gh pr create --body-file pr.md",
            f.root()
        );
        let p = preview(&cmd, &other_s, true);
        assert_eq!(p.root, f.root());
        assert_eq!(summary_text(&p), Some("from the shell's folder"), "{p:?}");

        let cmd = format!(
            "cd {} && git push origin main && cd {other_s} && gh pr create --body-file pr.md",
            f.root()
        );
        let p = preview(&cmd, &f.root(), true);
        assert_eq!(p.root, f.root());
        assert_eq!(summary_text(&p), Some("from the shell's folder"), "{p:?}");

        std::fs::remove_file(other.join("pr.md")).unwrap();
        let cmd = format!(
            "git -C {} push origin main && gh pr create --body-file pr.md",
            f.root()
        );
        let s = preview(&cmd, &other_s, true).pr_summary.unwrap();
        assert_eq!(s.text, None);
        assert!(s.note.unwrap().contains("doesn't exist"));
    }

    #[test]
    fn summary_only_honors_a_refusal() {
        let f = Fixture::new("i5");
        std::fs::write(f.work.join("pr.md"), "wrong folder's file").unwrap();
        let p = f.preview("cd \"$WT\" && gh pr edit --body-file pr.md");
        assert!(unavailable(&p).contains("shell variable"), "{p:?}");
        assert!(p.pr_summary.is_none(), "{p:?}");
        assert!(!p.pushes);
    }

    #[test]
    fn stale_body_file_is_not_shown() {
        let f = Fixture::new("i2");
        std::fs::write(f.work.join("pr.md"), "stale").unwrap();
        for cmd in [
            "cat > pr.md <<'EOF'\nfresh\nEOF\ngh pr edit --body-file pr.md",
            "echo fresh > pr.md && gh pr edit --body-file pr.md",
        ] {
            let s = f.preview(cmd).pr_summary.unwrap();
            assert_eq!(s.text, None, "{cmd}");
            assert_eq!(s.note.as_deref(), Some(WRITES_BODY), "{cmd}");
        }
    }
}
