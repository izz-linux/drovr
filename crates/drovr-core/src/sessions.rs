//! Claude Code's saved sessions, so an agent can be started from one. Reached
//! through `harness::Claude` (`find_session`, `recent_sessions`,
//! `session_file`).
//!
//! Claude Code keeps each session as `<config>/projects/<folder>/<id>.jsonl`,
//! where `<config>` is `$CLAUDE_CONFIG_DIR` or `~/.claude`. Every line records
//! the session's working directory (`cwd`).

use crate::config::user_home;
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SavedSession {
    pub id: String,
    /// The directory the session ran in; `claude --resume` must run there.
    pub cwd: String,
    /// The session's first prompt, shortened, to recognise it by.
    pub first_prompt: String,
    pub modified: f64,
}

fn projects_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".claude"))
        .join("projects")
}

/// Session IDs are UUIDs; anything else could reach outside the projects folder.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .find_map(|p| p["text"].as_str().map(String::from)),
        _ => None,
    }
}

/// Read the head of a session file for its directory and first real prompt.
fn read(path: &Path) -> Option<SavedSession> {
    let id = path.file_stem()?.to_str()?.to_string();
    let modified = path
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    let mut cwd = String::new();
    let mut first_prompt = String::new();
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(200).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = v["cwd"].as_str() {
                cwd = c.to_string();
            }
        }
        if first_prompt.is_empty() && v["type"] == "user" && v["isMeta"] != true {
            if let Some(t) = text_of(&v["message"]["content"]) {
                // Skip command wrappers and tool results, which start with markup.
                if !t.trim_start().starts_with('<') && !t.trim().is_empty() {
                    first_prompt = t
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(160)
                        .collect();
                }
            }
        }
        if !cwd.is_empty() && !first_prompt.is_empty() {
            break;
        }
    }
    if cwd.is_empty() {
        return None;
    }
    Some(SavedSession {
        id,
        cwd,
        first_prompt,
        modified,
    })
}

/// The transcript file of a session, by ID.
pub fn file(id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    std::fs::read_dir(projects_dir())
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

/// Find a session by ID.
pub fn find(id: &str) -> Option<SavedSession> {
    if !valid_id(id) {
        return None;
    }
    std::fs::read_dir(projects_dir())
        .ok()?
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
        .and_then(|p| read(&p))
}

/// The most recently used sessions, newest first.
pub fn recent(limit: usize) -> Vec<SavedSession> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(projects_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|dir| {
            std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(valid_id))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
        .into_iter()
        .take(limit)
        .filter_map(|(_, p)| read(&p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_finds_sessions() {
        let root = std::env::temp_dir().join(format!("drovr-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("projects/-Users-me-code-api");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "225e9ac0-9c9c-4f7c-927e-49054bff82a0";
        std::fs::write(
            dir.join(format!("{id}.jsonl")),
            [
                r#"{"type":"summary","summary":"x"}"#,
                r#"{"type":"user","cwd":"/Users/me/code/api","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"}}"#,
                r#"{"type":"user","cwd":"/Users/me/code/api","message":{"content":[{"type":"text","text":"Coordinate the   pay-api rollout"}]}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &root);

        let s = find(id).unwrap();
        assert_eq!(
            (s.cwd.as_str(), s.first_prompt.as_str()),
            ("/Users/me/code/api", "Coordinate the pay-api rollout")
        );
        assert!(find("../../etc/passwd").is_none());
        assert!(find("00000000-0000-0000-0000-000000000000").is_none());
        assert_eq!(recent(10).len(), 1);

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}
