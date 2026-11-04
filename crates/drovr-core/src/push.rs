//! Reading what a shell command would push: the repo, remote and branch of a
//! `git push`, and the title and body of a `gh pr create` or `gh pr edit`.
//! It only parses the text; nothing here runs the command.

use crate::review::words;

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

const MAX_DEPTH: usize = 4;

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
    dir_unknown: bool,
    committed: bool,
    add: Add,
}

impl Scan {
    fn refuse(&mut self, why: impl Into<String>) {
        if self.p.unpreviewable.is_none() {
            self.p.unpreviewable = Some(why.into());
        }
    }

    fn run(&mut self, cmd: &str, depth: usize) {
        let text = strip_heredocs(cmd);
        let words = strip_redirects(words(&text));
        for seg in words.split(|w| is_separator(w)) {
            self.segment(seg, depth);
        }
    }

    fn segment(&mut self, seg: &[String], depth: usize) {
        let skip = seg.iter().take_while(|w| is_assignment(w)).count();
        let Some((prog, args)) = seg[skip..].split_first() else {
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
            "git" => self.git(args),
            "gh" => self.gh(args),
            _ => {}
        }
    }

    fn cd(&mut self, args: &[String]) {
        if self.found {
            return;
        }
        let Some(d) = args.iter().find(|a| !a.starts_with('-')) else {
            return;
        };
        if unknowable(d) {
            self.dir_unknown = true;
        } else {
            self.p.dir = Some(join(self.p.dir.as_deref(), d));
        }
    }

    fn git(&mut self, args: &[String]) {
        let mut dir = self.p.dir.clone();
        let mut dir_unknown = self.dir_unknown;
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
                a if unknowable(a) => self.refuse("This push follows a git add of unknown files."),
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
        if dir_unknown || self.dir_unknown {
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
        self.p.commits_first = self.committed;
        if self.committed {
            self.p.add = self.add.clone();
        }

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

    fn gh(&mut self, args: &[String]) {
        let [pr, action, rest @ ..] = args else {
            return;
        };
        if pr != "pr" || !matches!(action.as_str(), "create" | "edit") {
            return;
        }
        let create = action == "create";
        self.found(false);
        if create && !self.pushed {
            self.p.pushes = true;
            self.p.commits_first = self.committed;
            if self.committed {
                self.p.add = self.add.clone();
            }
        }
        let mut j = 0;
        while j < rest.len() {
            if let Some(v) = value(rest, &mut j, &["--body-file", "-F"]) {
                self.p.body = if v == "-" {
                    Body::Unreadable("The body comes from standard input.".into())
                } else if unknowable(v) {
                    Body::Unreadable("The body file is set by a shell variable.".into())
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

/// Words that need the shell to expand them.
fn unknowable(w: &str) -> bool {
    w.contains('$') || w.contains('`')
}

fn join(cur: Option<&str>, d: &str) -> String {
    match cur {
        Some(c) if !d.starts_with('/') && !d.starts_with('~') => {
            format!("{}/{d}", c.trim_end_matches('/'))
        }
        _ => d.to_string(),
    }
}

/// Drop redirections (`> f`, `2>&1`, `<<EOF`) so they aren't read as
/// arguments. A word after a flag that takes a value is left alone: it's
/// the value (`--body '<b>x</b>'`).
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
            if !bare {
                i += 1;
            } else if words.get(i + 1).map(String::as_str) == Some("&") {
                i += 3;
            } else {
                i += 2;
            }
            continue;
        }
        out.push(w.clone());
        i += 1;
    }
    out
}

/// Remove heredoc bodies, line by line: after a line with an unquoted
/// `<<[-]['"]?DELIM`, drop the lines through the one equal to `DELIM`. A
/// `<<` inside quotes is left alone.
fn strip_heredocs(cmd: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut quote: Option<char> = None;
    let mut lines = cmd.split('\n');
    while let Some(line) = lines.next() {
        out.push(line);
        let cs: Vec<char> = line.chars().collect();
        let mut pending: Vec<(String, bool)> = Vec::new();
        let mut k = 0;
        while k < cs.len() {
            let c = cs[k];
            match quote {
                Some(q) if c == q => quote = None,
                Some('"') if c == '\\' => k += 1,
                Some(_) => {}
                None => match c {
                    '\'' | '"' => quote = Some(c),
                    '\\' => k += 1,
                    '<' if cs.get(k + 1) == Some(&'<') => {
                        if cs.get(k + 2) == Some(&'<') {
                            k += 2;
                        } else if let Some((delim, dash, end)) = heredoc_delim(&cs, k + 2) {
                            pending.push((delim, dash));
                            k = end;
                        } else {
                            k += 1;
                        }
                    }
                    _ => {}
                },
            }
            k += 1;
        }
        for (delim, dash) in pending {
            for l in lines.by_ref() {
                let l = if dash { l.trim_start_matches('\t') } else { l };
                if l == delim {
                    break;
                }
            }
        }
    }
    out.join("\n")
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
                |p| assert_eq!(p.body, Body::File("pr.md".into())),
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
    fn commands_before_a_push_do_not_leak() {
        let p = parse("git -C other status && git push origin a").unwrap();
        assert_eq!(p.dir, None);
        let p = parse("git push origin a && cd /late").unwrap();
        assert_eq!(p.dir, None);
    }
}
