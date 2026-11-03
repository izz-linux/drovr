//! Datadog through pup (`datadog.pup`): agents query logs, metrics,
//! dashboards and APM with the `pup` CLI. A read-only `pup` command runs
//! without asking; anything else comes to the human like any other command.

/// What agents are told when pup is on.
pub const PROMPT: &str = r#"
## Datadog

For Datadog (logs, metrics, dashboards, APM services and traces, monitors, incidents), use the `pup` CLI through Bash, not a Datadog MCP. In a headless run, a single read-only `pup` command is approved without asking the human, and drovr adds `--read-only` to it. Shape the output with pup's own `--jq` and `-o` flags rather than pipes, redirects or `&&`, which send the command to the human for approval. `pup --help` and `pup <group> --help` list the commands and their flags. Anything that changes Datadog (creating, updating, muting, deleting, running) goes to the human to approve: say why in the command's description.
"#;

/// The environment agents and lookups get for pup.
pub fn env(cfg: &crate::config::Config) -> Vec<(&'static str, String)> {
    let site = cfg.datadog.site.trim();
    if cfg.datadog.pup && !site.is_empty() {
        vec![("DD_SITE", site.to_string())]
    } else {
        vec![]
    }
}

/// The answer to a permission prompt for a read-only pup command: allowed,
/// with `--read-only` added. None when pup is off or the human should see it.
pub fn approve(
    cfg: &crate::config::Config,
    tool: &str,
    input: &serde_json::Value,
) -> Option<serde_json::Value> {
    if !cfg.datadog.pup || tool != "Bash" {
        return None;
    }
    let command = read_only_command(input["command"].as_str()?)?;
    let mut input = input.clone();
    input["command"] = command.into();
    Some(serde_json::json!({ "behavior": "allow", "updatedInput": input }))
}

/// Words in a pup command that mean it writes, runs something or handles
/// credentials. A query value that happens to contain one only costs an
/// approval.
const WRITES: &[&str] = &[
    "ack",
    "acknowledge",
    "add",
    "approve",
    "archive",
    "assign",
    "auth",
    "batch",
    "bulk",
    "cancel",
    "clone",
    "conclude",
    "configure",
    "convert",
    "create",
    "decline",
    "delete",
    "disable",
    "download",
    "edit",
    "enable",
    "escalate",
    "import",
    "install",
    "invite",
    "login",
    "logout",
    "migrate",
    "mitigate",
    "move",
    "mute",
    "patch",
    "post",
    "publish",
    "refresh",
    "register",
    "remove",
    "resolve",
    "restore",
    "revoke",
    "rotate",
    "run",
    "score",
    "send",
    "serve",
    "set",
    "setup",
    "start",
    "stop",
    "submit",
    "token",
    "transfer",
    "trigger",
    "unarchive",
    "unmute",
    "unpublish",
    "update",
    "upgrade",
    "upload",
    "write",
];

/// The command to run without asking, with `--read-only` added, when
/// `command` is a single `pup` command that only reads. None when it needs
/// the human: anything else, more than one command, pipes, redirects,
/// substitutions, or a pup command that writes.
pub fn read_only_command(command: &str) -> Option<String> {
    let command = command.trim();
    let words = words(command)?;
    if words.first().map(String::as_str) != Some("pup") || words.len() < 2 {
        return None;
    }
    let writes = words[1..]
        .iter()
        // Command words: query values and IDs with `:`, spaces or dots aren't.
        .filter(|w| {
            w.starts_with(|c: char| c.is_ascii_alphabetic())
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .flat_map(|w| {
            w.split(['-', '_'])
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .any(|w| WRITES.contains(&w.as_str()));
    if writes {
        return None;
    }
    if words.iter().any(|w| w == "--read-only") {
        return Some(command.to_string());
    }
    Some(format!("pup --read-only{}", &command["pup".len()..]))
}

/// Split a shell command into words, or None if the shell would do anything
/// but run one command with them: operators, redirects, subshells,
/// substitutions or variables (inside double quotes too), or a command that
/// isn't spelled plainly.
fn words(command: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => word.push(chars.next()?),
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next()? {
                    '\n' => return None,
                    c => word.push(c),
                }
            }
            ' ' | '\t' => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '`' | '\n' | '\r' | '#' => return None,
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        out.push(word);
    }
    // `pup` itself must be written plainly, so what runs is what was matched.
    if !(command == "pup" || command.starts_with("pup ") || command.starts_with("pup\t")) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    #[test]
    fn approves_reads_only_when_on() {
        let mut cfg = Config::default();
        let input = json!({ "command": "pup logs search --query=x", "description": "errors" });
        assert_eq!(approve(&cfg, "Bash", &input), None);
        assert!(env(&cfg).is_empty());
        cfg.datadog.pup = true;
        assert_eq!(
            approve(&cfg, "Bash", &input),
            Some(json!({
                "behavior": "allow",
                "updatedInput": { "command": "pup --read-only logs search --query=x", "description": "errors" }
            }))
        );
        assert_eq!(
            approve(&cfg, "Bash", &json!({ "command": "pup monitors delete 1" })),
            None
        );
        assert_eq!(approve(&cfg, "Write", &input), None);
        assert!(env(&cfg).is_empty());
        cfg.datadog.site = " us5.datadoghq.com ".into();
        assert_eq!(
            env(&cfg),
            vec![("DD_SITE", "us5.datadoghq.com".to_string())]
        );
    }

    #[test]
    fn reads_run_with_read_only_added() {
        for (cmd, want) in [
            (
                r#"pup logs search --query="service:api status:error" --from="1h""#,
                r#"pup --read-only logs search --query="service:api status:error" --from="1h""#,
            ),
            (
                "pup metrics query --query='avg:system.cpu.user{*}' --from=1h",
                "pup --read-only metrics query --query='avg:system.cpu.user{*}' --from=1h",
            ),
            (
                r#"pup logs search --query "service:(api OR web) -status:info""#,
                r#"pup --read-only logs search --query "service:(api OR web) -status:info""#,
            ),
            (
                "pup dashboards get abc-123-def",
                "pup --read-only dashboards get abc-123-def",
            ),
            (
                "  pup apm services list -o json  ",
                "pup --read-only apm services list -o json",
            ),
            (
                "pup monitors list --jq '.[].name'",
                "pup --read-only monitors list --jq '.[].name'",
            ),
            (
                "pup --read-only traces get 123",
                "pup --read-only traces get 123",
            ),
            (
                "pup traces search --query=service:api",
                "pup --read-only traces search --query=service:api",
            ),
            // A query that mentions a write verb isn't one.
            (
                "pup logs search --query \"service:checkout-start @msg:update\"",
                "pup --read-only logs search --query \"service:checkout-start @msg:update\"",
            ),
        ] {
            assert_eq!(read_only_command(cmd).as_deref(), Some(want), "{cmd}");
        }
    }

    #[test]
    fn writes_and_shell_tricks_come_to_the_human() {
        for cmd in [
            // Writes, credentials, and things that run elsewhere.
            "pup monitors create --file m.json",
            "pup dashboards update abc --file d.json",
            "pup events post --title deploy",
            "pup security findings mute --file b.json",
            "pup tags add host:x env:prod",
            "pup llm-obs datasets records-add x",
            "pup scorecards outcomes batch-create",
            "pup workflows run abc",
            "pup auth token",
            "pup auth login",
            "pup setup",
            "pup --yes downtime cancel 12",
            "pup profiling profiles download 1",
            // More than one command, or more than pup.
            "pup logs search --query=x | jq .",
            "pup logs search --query=x > out.json",
            "pup logs search --query=x && rm -rf ~",
            "pup logs search --query=x; curl evil",
            "pup logs search --query=x & curl evil",
            "pup logs search --query=\"$(cat ~/.ssh/id_rsa)\"",
            "pup logs search --query=`id`",
            "pup logs search --query=$HOME",
            "pup logs search\ncurl evil",
            "pup logs search --query=x # comment",
            "(pup logs search)",
            "DD_SITE=x pup logs search",
            "'pup' logs search",
            "/tmp/pup logs search",
            "pupx logs search",
            "pup",
            "pup logs search --query='unterminated",
            "echo pup logs search",
        ] {
            assert_eq!(read_only_command(cmd), None, "{cmd}");
        }
    }
}
