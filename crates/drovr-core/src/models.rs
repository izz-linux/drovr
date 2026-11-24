//! Model names for reporting. The same model goes by several names: a
//! gateway's path in OpenCode, Claude Code's id, or an alias such as `opus`.
//! Agents always start with the name as configured; these are only for
//! adding up what a model did.

use std::collections::BTreeSet;

/// Claude Code's aliases, which mean a different version over time.
const ALIASES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

/// Gateway entries that pick a model themselves.
const ROUTERS: &[&str] = &["cloudflare/auto", "unbiased/pareto"];

fn lowered(name: &str) -> String {
    let name = name.trim().to_lowercase();
    match name.split_once('[') {
        Some((before, _)) => before.to_string(),
        None => name,
    }
}

/// One name for a model, however it was written. A router or an alias whose
/// version isn't known says so.
pub fn canonical(name: &str) -> String {
    let full = lowered(name);
    let last = full.rsplit('/').next().unwrap_or("");
    if ROUTERS.iter().any(|r| full.ends_with(r)) {
        return format!("{last} (routed)");
    }
    if ALIASES.contains(&last) {
        return format!("claude-{last} (version unknown)");
    }
    if !last.starts_with("claude-") {
        return last.to_string();
    }
    let id = last.replace('.', "-");
    match id.rsplit_once('-') {
        Some((before, date)) if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) => {
            before.to_string()
        }
        _ => id,
    }
}

/// The model a run worked as, from the name it was started with and the
/// models it reported.
pub fn main_model(asked: &str, used: &[String]) -> String {
    let used: BTreeSet<String> = used.iter().map(|m| canonical(m)).collect();
    let only = |mut of: Vec<&String>| (of.len() == 1).then(|| of.remove(0).clone());
    if let Some(model) = only(used.iter().collect()) {
        return model;
    }
    let alias = lowered(asked);
    if ALIASES.contains(&alias.as_str()) {
        let family = format!("claude-{alias}-");
        return only(used.iter().filter(|m| m.starts_with(&family)).collect())
            .unwrap_or_else(|| canonical(asked));
    }
    if !alias.is_empty() {
        return canonical(asked);
    }
    if used.is_empty() {
        "default".into()
    } else {
        "several models".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_name_per_model() {
        for (name, expected) in [
            (
                "cloudflare-ai-gateway/anthropic/claude-opus-5.5",
                "claude-opus-5-5",
            ),
            ("anthropic/claude-haiku-4.5", "claude-haiku-4-5"),
            ("claude-opus-5-5", "claude-opus-5-5"),
            ("claude-opus-5-5[1m]", "claude-opus-5-5"),
            ("claude-haiku-4-5-20251001", "claude-haiku-4-5"),
            (" Claude-Sonnet-5-5 ", "claude-sonnet-5-5"),
            ("cloudflare-ai-gateway/openai/gpt-5.5", "gpt-5.5"),
            (
                "cloudflare-ai-gateway/xai/grok-4.20-0309-reasoning",
                "grok-4.20-0309-reasoning",
            ),
            ("cloudflare-ai-gateway/cloudflare/auto", "auto (routed)"),
            ("cloudflare-ai-gateway/unbiased/pareto", "pareto (routed)"),
            ("Opus", "claude-opus (version unknown)"),
            ("sonnet[1m]", "claude-sonnet (version unknown)"),
            ("", ""),
        ] {
            assert_eq!(canonical(name), expected, "{name}");
        }
    }

    #[test]
    fn a_runs_main_model() {
        for (asked, used, expected) in [
            (
                "opus",
                &["claude-haiku-5-5", "claude-opus-5-5"][..],
                "claude-opus-5-5",
            ),
            (
                "sonnet",
                &["claude-opus-5-5", "claude-sonnet-5-5"],
                "claude-sonnet-5-5",
            ),
            ("opus", &[], "claude-opus (version unknown)"),
            (
                "opus",
                &["claude-opus-4-8", "claude-opus-5-5"],
                "claude-opus (version unknown)",
            ),
            ("gw/anthropic/claude-opus-5.5", &[], "claude-opus-5-5"),
            ("", &["claude-sonnet-5-5"], "claude-sonnet-5-5"),
            // Switched in the session: what it ran on counts.
            ("opus", &["claude-sonnet-5-5"], "claude-sonnet-5-5"),
            (
                "claude-opus-5-5",
                &["claude-sonnet-5-5"],
                "claude-sonnet-5-5",
            ),
            (
                "",
                &["claude-opus-5-5", "claude-opus-5-5[1m]"],
                "claude-opus-5-5",
            ),
            (
                "",
                &["claude-opus-5-5", "claude-sonnet-5-5"],
                "several models",
            ),
            ("", &[], "default"),
        ] {
            let used: Vec<String> = used.iter().map(|m| m.to_string()).collect();
            assert_eq!(main_model(asked, &used), expected, "{asked} {used:?}");
        }
    }
}
