//! The record: every step of a ticket's work, kept for the retro.
//!
//! Each ticket has a folder in the plans folder: `<plans>/<group>/<TICKET>/`
//! for a group's ticket, `<plans>/<TICKET>/` for a solo agent's. Its
//! `ledger.jsonl` holds one JSON event per line, appended and never
//! rewritten, so it reads in any editor and diffs cleanly in git. The app
//! and every agent's MCP server write to it, so each append holds a lock.

use crate::config::Config;
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "ledger.jsonl";

/// Ticket keys in `text`, uppercased and in order: `SRE-3230, sre-3253`
/// gives `SRE-3230` and `SRE-3253`.
pub fn tickets_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        let Some((project, number)) = word.rsplit_once('-') else {
            continue;
        };
        let ok = project.len() >= 2
            && project
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && project.chars().all(|c| c.is_ascii_alphanumeric())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit());
        let key = word.to_uppercase();
        if ok && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// A ticket's folder: in `folder` (a group's plans folder) when given, else
/// in the plans folder itself.
pub fn dir(cfg: &Config, folder: &str, ticket: &str) -> PathBuf {
    let root = cfg.reports.path();
    let base = if folder.is_empty() {
        root
    } else {
        root.join(folder)
    };
    base.join(ticket)
}

/// Append `event` to `ticket`'s record, stamped with the time.
pub fn append(cfg: &Config, folder: &str, ticket: &str, event: &Value) -> Result<()> {
    if !folder.is_empty() {
        crate::reports::check_folder(folder)?;
    }
    crate::reports::check_folder(ticket)?;
    let dir = dir(cfg, folder, ticket);
    std::fs::create_dir_all(&dir)?;
    let ts = crate::store::now();
    let mut e = json!({ "ts": ts, "at": iso(ts), "ticket": ticket });
    if let (Some(e), Some(fields)) = (e.as_object_mut(), event.as_object()) {
        for (k, v) in fields {
            e.insert(k.clone(), v.clone());
        }
    }
    let line = redact(&serde_json::to_string(&e)?) + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE))?;
    let _lock = Lock::exclusive(&f);
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Holds an flock until dropped, so lines from several processes don't
/// interleave.
struct Lock(i32);

impl Lock {
    fn exclusive(f: &std::fs::File) -> Lock {
        use std::os::unix::io::AsRawFd;
        let fd = f.as_raw_fd();
        unsafe { libc::flock(fd, libc::LOCK_EX) };
        Lock(fd)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

/// Every event in a record, oldest first. Lines that aren't JSON (a
/// hand edit, a torn write) are skipped.
pub fn read(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join(FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A ticket that has a record.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub ticket: String,
    /// The group's plans folder it's in; empty for the plans folder itself.
    pub folder: String,
    pub path: String,
    pub events: usize,
    pub updated: f64,
}

/// Every record in the plans folder, most recently updated first.
pub fn list(cfg: &Config) -> Vec<Record> {
    let root = cfg.reports.path();
    let mut out = vec![];
    let mut add = |dir: &Path, folder: &str, ticket: &str| {
        let file = dir.join(FILE);
        let Ok(meta) = std::fs::metadata(&file) else {
            return;
        };
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        out.push(Record {
            ticket: ticket.to_string(),
            folder: folder.to_string(),
            path: dir.display().to_string(),
            events: text.lines().filter(|l| !l.trim().is_empty()).count(),
            updated: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        });
    };
    for entry in subdirs(&root) {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_record(&entry.path()) {
            add(&entry.path(), "", &name);
            continue;
        }
        for t in subdirs(&entry.path()) {
            add(&t.path(), &name, &t.file_name().to_string_lossy());
        }
    }
    out.sort_by(|a, b| b.updated.total_cmp(&a.updated));
    out
}

/// Whether `dir` is a ticket's record folder rather than a group's plans folder.
pub fn is_record(dir: &Path) -> bool {
    dir.join(FILE).is_file()
}

fn subdirs(dir: &Path) -> impl Iterator<Item = std::fs::DirEntry> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
}

/// Every event for `ticket`, from every folder that has a record of it,
/// oldest first.
pub fn timeline(cfg: &Config, ticket: &str) -> Vec<Value> {
    let ticket = ticket.trim().to_uppercase();
    let mut events: Vec<Value> = list(cfg)
        .into_iter()
        .filter(|r| r.ticket == ticket)
        .flat_map(|r| read(Path::new(&r.path)))
        .collect();
    events.sort_by(|a, b| {
        let t = |v: &Value| v["ts"].as_f64().unwrap_or(0.0);
        t(a).total_cmp(&t(b))
    });
    events
}

/// `ts` (seconds since 1970) as `2026-10-29T03:12:07Z`.
pub fn iso(ts: f64) -> String {
    let secs = ts.max(0.0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Tokens that look like credentials, by their prefix, and how many
/// characters must follow for it to count.
const SECRETS: &[(&str, usize)] = &[
    ("github_pat_", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("glpat-", 16),
    ("xoxb-", 16),
    ("xoxp-", 16),
    ("xoxa-", 16),
    ("xoxs-", 16),
    ("sk-ant-", 20),
    ("AKIA", 16),
    ("ASIA", 16),
    ("Bearer ", 20),
];

/// Replace anything that looks like a credential with `[redacted]`. Agents
/// paste command output and logs into what they send, and the record is
/// meant to be shared and kept. What's replaced never includes quotes or
/// backslashes, so a JSON line stays valid.
pub fn redact(s: &str) -> String {
    let token = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    'scan: while i < s.len() {
        for (prefix, min) in SECRETS {
            if s[i..].starts_with(prefix)
                && (i == 0 || !s[..i].ends_with(|c: char| c.is_ascii_alphanumeric()))
            {
                let rest = &s[i + prefix.len()..];
                let n = rest.find(|c: char| !token(c)).unwrap_or(rest.len());
                if n >= *min {
                    out.push_str(prefix.trim_end());
                    out.push_str(if prefix.ends_with(' ') { " " } else { "" });
                    out.push_str("[redacted]");
                    i += prefix.len() + n;
                    continue 'scan;
                }
            }
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ticket_keys() {
        assert_eq!(tickets_in("SRE-3230, sre-3253"), ["SRE-3230", "SRE-3253"]);
        assert_eq!(tickets_in("fix SRE-3229 (again: SRE-3229)"), ["SRE-3229"]);
        assert!(tickets_in("").is_empty());
        // Agent names, one-letter projects and bare numbers aren't keys.
        assert!(tickets_in("pay-412-lead v-2 3-4").is_empty());
    }

    #[test]
    fn formats_utc_times() {
        assert_eq!(iso(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_793_243_527.9), "2026-10-29T03:12:07Z");
        assert_eq!(iso(951_782_400.0), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn redacts_credentials() {
        let s = r#"{"body":"token ghp_abcdefghijklmnopqrstuvwxyz0123 and Bearer eyJhbGciOiJIUzI1NiJ9abc, AKIAABCDEFGHIJKLMNOP"}"#;
        let r = redact(s);
        assert_eq!(
            r,
            r#"{"body":"token ghp_[redacted] and Bearer [redacted], AKIA[redacted]"}"#
        );
        assert!(serde_json::from_str::<Value>(&r).is_ok());
        // Short or embedded look-alikes stay.
        assert_eq!(redact("ghp_short and TASKIA123"), "ghp_short and TASKIA123");
        assert_eq!(redact("héllo wörld"), "héllo wörld");
    }

    #[test]
    fn appends_lists_and_reads_records() {
        let dir = std::env::temp_dir().join(format!("drovr-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();

        append(
            &cfg,
            "sre-3229",
            "SRE-3229",
            &json!({ "kind": "run_start", "run": 1 }),
        )
        .unwrap();
        append(&cfg, "", "SRE-3180", &json!({ "kind": "decision" })).unwrap();
        append(&cfg, "other", "SRE-3229", &json!({ "kind": "message" })).unwrap();
        assert!(append(&cfg, "../x", "SRE-1", &json!({})).is_err());
        assert!(append(&cfg, "", "..", &json!({})).is_err());

        let records = list(&cfg);
        assert_eq!(records.len(), 3);
        assert!(records
            .iter()
            .any(|r| r.ticket == "SRE-3180" && r.folder.is_empty()));
        let t = timeline(&cfg, "sre-3229");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["kind"], "run_start");
        assert_eq!(t[0]["ticket"], "SRE-3229");
        assert!(t[0]["at"].as_str().unwrap().ends_with('Z'));
        assert!(is_record(&dir.join("SRE-3180")));
        assert!(!is_record(&dir.join("sre-3229")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_from_many_threads_stay_whole() {
        let dir = std::env::temp_dir().join(format!("drovr-ledger-mt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        let body = "x".repeat(10_000);
        std::thread::scope(|s| {
            for t in 0..8 {
                let (cfg, body) = (&cfg, &body);
                s.spawn(move || {
                    for i in 0..20 {
                        append(
                            cfg,
                            "",
                            "SRE-1",
                            &json!({ "kind": "m", "t": t, "i": i, "body": body }),
                        )
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(read(&dir.join("SRE-1")).len(), 160);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
