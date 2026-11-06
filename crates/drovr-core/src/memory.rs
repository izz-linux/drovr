//! Shared memory (`memory.dir`): every Claude Code agent reads the auto
//! memory in that folder, wherever the agent works. Leads and solo agents
//! write it; workers send what's worth keeping to their lead, so several
//! agents don't rewrite the same files at once.

use crate::config::{expand_tilde, Config};
use crate::store::Agent;
use serde_json::json;
use std::path::PathBuf;

/// The shared memory folder, or None when agents keep Claude Code's own
/// memory per project.
pub fn dir(cfg: &Config) -> Option<PathBuf> {
    let dir = cfg.memory.dir.trim();
    (!dir.is_empty()).then(|| PathBuf::from(expand_tilde(dir)))
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
        // `//` starts an absolute path in a permission rule. Claude Code applies
        // Edit rules to Write too, and warns about a Write rule on a path.
        let rule = format!("/{}/**", dir.display());
        s["permissions"] = json!({ "deny": [format!("Edit({rule})")] });
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
    fn off_unless_a_folder_is_set() {
        let mut cfg = Config::default();
        assert_eq!(dir(&cfg), None);
        assert_eq!(settings(&cfg, &agent("worker")), None);
        assert_eq!(prompt(&cfg, &agent("orchestrator")), "");
        cfg.memory.dir = "  ".into();
        assert_eq!(dir(&cfg), None);
        cfg.memory.dir = "~/team-memory".into();
        assert_eq!(
            dir(&cfg),
            Some(crate::config::user_home().join("team-memory"))
        );
    }

    #[test]
    fn only_leads_and_solo_agents_write_it() {
        let mut cfg = Config::default();
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
                "permissions": { "deny": ["Edit(//m/memory/**)"] }
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
