//! Shared memory (`memory.shared`): every Claude Code agent reads one auto
//! memory, the one Claude Code keeps for `defaults.cwd` (or `memory.dir`),
//! wherever the agent works. Leads and solo agents write it; workers send
//! what's worth keeping to their lead, so several agents don't rewrite the
//! same files at once.

use crate::config::{expand_tilde, Config};
use crate::store::Agent;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The shared memory folder, or None when agents keep Claude Code's own
/// memory per folder.
pub fn dir(cfg: &Config) -> Option<PathBuf> {
    if !cfg.memory.shared {
        return None;
    }
    let dir = cfg.memory.dir.trim();
    if !dir.is_empty() {
        return Some(PathBuf::from(expand_tilde(dir)));
    }
    let cwd = PathBuf::from(cfg.default_cwd());
    let root = repo_root(&cwd).unwrap_or(cwd);
    Some(
        crate::config::user_home()
            .join(".claude/projects")
            .join(project_key(&root))
            .join("memory"),
    )
}

/// The folder name Claude Code gives a project under `~/.claude/projects`:
/// its path with everything but letters and digits turned into `-`.
pub fn project_key(path: &Path) -> String {
    path.display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The git repo `dir` is in, which Claude Code keys memory by.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    common.parent().map(Path::to_path_buf)
}

/// Whether the agent writes the shared memory: leads and solo agents do,
/// workers don't.
pub fn writes(agent: &Agent) -> bool {
    agent.role != "worker"
}

/// Claude Code's `--settings` for the agent: the shared memory folder, and
/// for a worker, Write and Edit refused there.
pub fn settings(cfg: &Config, agent: &Agent) -> Option<String> {
    let dir = dir(cfg)?;
    let mut s = json!({ "autoMemoryDirectory": dir.display().to_string() });
    if !writes(agent) {
        // `//` starts an absolute path in a permission rule.
        let rule = format!("/{}/**", dir.display());
        s["permissions"] = json!({ "deny": [format!("Write({rule})"), format!("Edit({rule})")] });
    }
    Some(s.to_string())
}

/// What the agent is told about the shared memory.
pub fn prompt(cfg: &Config, agent: &Agent) -> String {
    let Some(dir) = dir(cfg) else {
        return String::new();
    };
    let dir = dir.display();
    if !writes(agent) {
        return format!(
            "\n## Memory\n\nYour auto memory ({dir}) is shared by every drovr agent, and your lead keeps it. Read it, but don't write to it: drovr refuses writes there from workers. When you learn something worth remembering on later tickets (a correction from the human, a fact about the systems, a pitfall), send it to your lead with `send_message`, starting with \"For memory:\".\n"
        );
    }
    let workers = if agent.role == "orchestrator" {
        " Your workers send you what they think is worth keeping, starting with \"For memory:\"; save what will help later tickets."
    } else {
        ""
    };
    format!(
        "\n## Memory\n\nYour auto memory ({dir}) is shared by every drovr agent, and only leads and solo agents like you write it.{workers} Keep MEMORY.md, its index, under 200 lines: past that it stops loading. Another agent may be writing it too, so read a file just before you change it, and edit it rather than rewriting it.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(role: &str) -> Agent {
        Agent {
            name: "a".into(),
            role: role.into(),
            ..Agent::default()
        }
    }

    #[test]
    fn names_projects_as_claude_code_does() {
        assert_eq!(
            project_key(Path::new(
                "/Users/Izz.Noland/develop/tools/src/github.com/stord"
            )),
            "-Users-Izz-Noland-develop-tools-src-github-com-stord"
        );
        assert_eq!(
            project_key(Path::new(
                "/Users/izz/Dropbox/develop/tools/src/github.com/izz-linux"
            )),
            "-Users-izz-Dropbox-develop-tools-src-github-com-izz-linux"
        );
    }

    #[test]
    fn off_unless_shared() {
        let cfg = Config::default();
        assert_eq!(dir(&cfg), None);
        assert_eq!(settings(&cfg, &agent("worker")), None);
        assert_eq!(prompt(&cfg, &agent("orchestrator")), "");
    }

    #[test]
    fn shares_the_memory_of_the_default_folder() {
        let base = std::env::temp_dir().join(format!("drovr-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let plain = base.join("stord");
        std::fs::create_dir_all(plain.join("api/src")).unwrap();
        let mut cfg = Config::default();
        cfg.memory.shared = true;
        cfg.defaults.cwd = plain.display().to_string();
        let home = crate::config::user_home().join(".claude/projects");
        assert_eq!(
            dir(&cfg),
            Some(home.join(project_key(&plain)).join("memory"))
        );
        // In a repo, Claude Code keys it by the repo.
        let repo = plain.join("api");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        cfg.defaults.cwd = repo.join("src").display().to_string();
        let root = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(
            dir(&cfg),
            Some(home.join(project_key(&root)).join("memory"))
        );
        // Or name the folder yourself.
        cfg.memory.dir = "/m/memory".into();
        assert_eq!(dir(&cfg), Some(PathBuf::from("/m/memory")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_leads_and_solo_agents_write_it() {
        let mut cfg = Config::default();
        cfg.memory.shared = true;
        cfg.memory.dir = "/m/memory".into();
        let parse =
            |s: Option<String>| serde_json::from_str::<serde_json::Value>(&s.unwrap()).unwrap();
        for role in ["orchestrator", "solo"] {
            assert_eq!(
                parse(settings(&cfg, &agent(role))),
                json!({ "autoMemoryDirectory": "/m/memory" }),
                "{role}"
            );
            assert!(prompt(&cfg, &agent(role)).contains("edit it rather than rewriting it"));
        }
        assert!(prompt(&cfg, &agent("orchestrator")).contains("Your workers send you"));
        assert!(!prompt(&cfg, &agent("solo")).contains("Your workers send you"));
        assert_eq!(
            parse(settings(&cfg, &agent("worker"))),
            json!({
                "autoMemoryDirectory": "/m/memory",
                "permissions": { "deny": ["Write(//m/memory/**)", "Edit(//m/memory/**)"] }
            })
        );
        let worker = prompt(&cfg, &agent("worker"));
        assert!(
            worker.contains("send it to your lead with `send_message`"),
            "{worker}"
        );
        assert!(worker.contains("(/m/memory)"), "{worker}");
    }
}
