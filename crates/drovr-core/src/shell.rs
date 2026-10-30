//! Small helpers for running shell commands and filling command templates.

use anyhow::{bail, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Replace `{name}` placeholders with shell-quoted values. `{herdr}` and other
/// keys listed in `raw` are inserted unquoted.
pub fn fill(template: &str, vars: &[(&str, &str)], raw: &[&str]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        let value = if raw.contains(key) {
            value.to_string()
        } else {
            quote(value)
        };
        out = out.replace(&format!("{{{key}}}"), &value);
    }
    out
}

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0 && !self.timed_out
    }

    /// stdout on success, otherwise an error carrying stderr.
    pub fn into_stdout(self, what: &str) -> Result<String> {
        if self.ok() {
            return Ok(self.stdout);
        }
        let reason = if self.timed_out {
            "timed out".to_string()
        } else {
            format!("exited {}", self.code)
        };
        bail!(
            "{what} {reason}: {}",
            self.stderr.trim().chars().take(2000).collect::<String>()
        )
    }
}

/// Run `sh -c script` in `cwd`, feeding `stdin`, killing it after `timeout`.
pub fn sh(
    script: &str,
    cwd: Option<&Path>,
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<Output> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            timed_out = true;
            unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Ok(Output {
        code: status.code().unwrap_or(-1),
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
        timed_out,
    })
}

pub fn which(program: &str) -> bool {
    sh(
        &format!("command -v {}", quote(program)),
        None,
        None,
        Duration::from_secs(5),
    )
    .map(|o| o.ok())
    .unwrap_or(false)
}

/// Pull the first JSON object or array out of free text (agents sometimes wrap
/// JSON in prose or code fences).
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(text.trim()) {
        return Some(v);
    }
    for (start, open) in text.char_indices().filter(|(_, c)| *c == '{' || *c == '[') {
        let close = if open == '{' { '}' } else { ']' };
        for (end, _) in text.char_indices().rev().filter(|(_, c)| *c == close) {
            if end <= start {
                break;
            }
            if let Ok(v) = serde_json::from_str(&text[start..=end]) {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_fills() {
        assert_eq!(quote("w1:p2"), "w1:p2");
        assert_eq!(quote("it's"), r"'it'\''s'");
        let s = fill(
            "{herdr} agent prompt {target} {text}",
            &[
                ("herdr", "herdr"),
                ("target", "w1:p2"),
                ("text", "hi there"),
            ],
            &["herdr"],
        );
        assert_eq!(s, "herdr agent prompt w1:p2 'hi there'");
    }

    #[test]
    fn extracts_json_from_prose() {
        let v = extract_json("Here you go:\n```json\n{\"a\": [1, 2]}\n```").unwrap();
        assert_eq!(v["a"][1], 2);
    }

    #[test]
    fn sh_times_out() {
        let o = sh("sleep 5", None, None, Duration::from_millis(200)).unwrap();
        assert!(o.timed_out);
        let o = sh("cat", None, Some("abc"), Duration::from_secs(5)).unwrap();
        assert_eq!(o.stdout, "abc");
    }
}
