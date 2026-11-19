//! Archive old plans and prune resolved Inbox items.
//!
//! Plans are only ever moved (`rename`, inside the plans folder), never
//! deleted. Symlinked top-level entries are left alone.

use crate::config::Config;
use crate::ledger::ARCHIVE;
use crate::store::Store;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

const DAY: f64 = 86400.0;
const FILE: &str = "cleanup.json";

#[derive(Serialize, Debug, Default)]
pub struct Outcome {
    /// (name in the plans folder, name under `archive/`).
    pub archived: Vec<(String, String)>,
    pub inbox_deleted: usize,
}

/// Archive old plans and prune resolved Inbox items. With `dry`, report what
/// would happen and change nothing.
pub fn run(cfg: &Config, store: &Store, now: f64, dry: bool) -> Result<Outcome> {
    let mut out = Outcome::default();
    if cfg.reports.archive_after_days > 0 {
        let cutoff = now - f64::from(cfg.reports.archive_after_days) * DAY;
        archive(cfg, store, cutoff, dry, &mut out)?;
    }
    if cfg.inbox.keep_days > 0 {
        let cutoff = now - f64::from(cfg.inbox.keep_days) * DAY;
        out.inbox_deleted = store.prune_items(cutoff, dry)?;
    }
    Ok(out)
}

fn archive(cfg: &Config, store: &Store, cutoff: f64, dry: bool, out: &mut Outcome) -> Result<()> {
    let root = cfg.reports.path();
    let Ok(read) = std::fs::read_dir(&root) else {
        return Ok(());
    };
    let live = live_folders(store)?;
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let archive_dir = root.join(ARCHIVE);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink()
            || name.starts_with('.')
            || name == ARCHIVE
            || name == "retros"
            || live.contains(&name)
        {
            continue;
        }
        let path = entry.path();
        if newest(&path) >= cutoff {
            continue;
        }
        let target = free_name(&archive_dir, &name, kind.is_dir());
        if !dry {
            std::fs::create_dir_all(&archive_dir)
                .with_context(|| format!("create {}", archive_dir.display()))?;
            std::fs::rename(&path, archive_dir.join(&target))
                .with_context(|| format!("archive {}", path.display()))?;
            let (from, to) = (path.display().to_string(), archive_dir.join(&target));
            let to = to.display().to_string();
            if kind.is_dir() {
                let sep = std::path::MAIN_SEPARATOR;
                store.move_report_meta(&format!("{from}{sep}"), &format!("{to}{sep}"))?;
            } else {
                store.move_report_meta(&from, &to)?;
            }
        }
        out.archived.push((name, target));
    }
    Ok(())
}

/// Folder names that belong to a group that still exists.
fn live_folders(store: &Store) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    for g in store.groups()? {
        names.insert(g.name.clone());
        let first = Path::new(&g.plans_dir).components().find_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().to_string()),
            _ => None,
        });
        if let Some(first) = first {
            names.insert(first);
        }
    }
    Ok(names)
}

/// The newest mtime (Unix seconds) of any file under `path`, or of `path`
/// itself when it's a file or holds none. Symlinks aren't followed.
fn newest(path: &Path) -> f64 {
    fn mtime(m: &std::fs::Metadata) -> f64 {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64())
    }
    fn walk(dir: &Path, best: &mut Option<f64>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                walk(&e.path(), best);
            } else if m.is_file() {
                *best = Some(best.map_or(mtime(&m), |b| b.max(mtime(&m))));
            }
        }
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return f64::MAX;
    };
    if !meta.is_dir() {
        return mtime(&meta);
    }
    let mut best = None;
    walk(path, &mut best);
    best.unwrap_or_else(|| mtime(&meta))
}

/// `name` if `dir` has nothing by that name, else `name-2`, `name-3`, ...
/// with the number before a file's extension.
fn free_name(dir: &Path, name: &str, is_dir: bool) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let p = Path::new(name);
    let (stem, ext) = if is_dir {
        (name.to_string(), String::new())
    } else {
        (
            p.file_stem()
                .map_or(name.to_string(), |s| s.to_string_lossy().to_string()),
            p.extension()
                .map_or(String::new(), |e| format!(".{}", e.to_string_lossy())),
        )
    };
    (2..)
        .map(|n| format!("{stem}-{n}{ext}"))
        .find(|c| !dir.join(c).exists())
        .expect("an unused name")
}

fn gate_file(home: &Path) -> PathBuf {
    home.join(FILE)
}

/// Whether a day has passed since the last automatic run.
pub fn due(home: &Path, now: f64) -> bool {
    std::fs::read_to_string(gate_file(home))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["last_run"].as_f64())
        .is_none_or(|last| now - last >= DAY)
}

pub fn mark_ran(home: &Path, now: f64) -> Result<()> {
    let file = gate_file(home);
    std::fs::write(&file, serde_json::json!({ "last_run": now }).to_string())
        .with_context(|| format!("write {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::Store;
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const DAY: f64 = 86400.0;

    struct Env {
        dir: PathBuf,
        plans: PathBuf,
        cfg: Config,
        store: Store,
        now: f64,
    }

    fn env(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("drovr-cleanup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plans = dir.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        let plans = plans.canonicalize().unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = plans.display().to_string();
        cfg.reports.archive_after_days = 30;
        let store = Store::open(&dir.join("t.db")).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        Env {
            dir,
            plans,
            cfg,
            store,
            now,
        }
    }

    /// Write `rel` under the plans folder, `days` old as of `e.now`.
    fn write(e: &Env, rel: &str, days: f64) -> PathBuf {
        let p = e.plans.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
        age(&p, e.now, days);
        p
    }

    fn age(p: &Path, now: f64, days: f64) {
        let t = UNIX_EPOCH + Duration::from_secs_f64(now - days * DAY);
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn names(o: &Outcome) -> Vec<(&str, &str)> {
        o.archived
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect()
    }

    fn set_item(e: &Env, id: i64, ts: f64, answered: Option<f64>, delivered: i64) {
        let db = Connection::open(e.dir.join("t.db")).unwrap();
        db.execute(
            "UPDATE inbox SET ts = ?1, answered_ts = ?2, delivered = ?3 WHERE id = ?4",
            rusqlite::params![ts, answered, delivered, id],
        )
        .unwrap();
    }

    fn add(e: &Env) -> i64 {
        e.store
            .add_item("a", "decision", "t", "", &[], &serde_json::json!({}))
            .unwrap()
    }

    #[test]
    fn archives_old_entries_whole() {
        let e = env("whole");
        write(&e, "old-group/SRE-1/ledger.jsonl", 40.0);
        write(&e, "loose.html", 40.0);
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(e
            .plans
            .join("archive/old-group/SRE-1/ledger.jsonl")
            .is_file());
        assert!(e.plans.join("archive/loose.html").is_file());
        assert!(!e.plans.join("old-group").exists());
        assert!(!e.plans.join("loose.html").exists());
        assert_eq!(
            names(&o),
            vec![("loose.html", "loose.html"), ("old-group", "old-group")]
        );
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn keeps_folders_with_a_fresh_file() {
        let e = env("fresh");
        write(&e, "busy/old.html", 90.0);
        write(&e, "busy/nested/new.html", 2.0);
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(e.plans.join("busy/old.html").is_file());
        assert!(o.archived.is_empty());
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn never_archives_reserved_or_active() {
        let e = env("reserved");
        write(&e, "archive/keep.html", 90.0);
        write(&e, "retros/r.html", 90.0);
        write(&e, ".hidden/h.html", 90.0);
        write(&e, "live/l.html", 90.0);
        write(&e, "custom/c.html", 90.0);
        e.store.add_group("live", "g", "", "", "", false).unwrap();
        e.store
            .add_group("other", "g", "", "", "custom", false)
            .unwrap();
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(o.archived.is_empty(), "{:?}", o.archived);
        for p in [
            "archive/keep.html",
            "retros/r.html",
            ".hidden/h.html",
            "live/l.html",
            "custom/c.html",
        ] {
            assert!(e.plans.join(p).is_file(), "{p}");
        }
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn collision_gets_a_suffix() {
        let e = env("collide");
        write(&e, "archive/x/f.html", 90.0);
        write(&e, "archive/a.html", 90.0);
        write(&e, "x/g.html", 40.0);
        write(&e, "a.html", 40.0);
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(e.plans.join("archive/x-2/g.html").is_file());
        assert!(e.plans.join("archive/a-2.html").is_file());
        assert!(e.plans.join("archive/x/f.html").is_file());
        assert_eq!(names(&o), vec![("a.html", "a-2.html"), ("x", "x-2")]);
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn report_meta_follows_the_move() {
        let e = env("meta");
        let p = write(&e, "old/r.html", 40.0);
        let keep = write(&e, "new/k.html", 1.0);
        e.store
            .publish_report(&p.display().to_string(), "a", "T", "S")
            .unwrap();
        e.store
            .publish_report(&keep.display().to_string(), "a", "T", "S")
            .unwrap();
        run(&e.cfg, &e.store, e.now, false).unwrap();
        let paths: Vec<String> = e
            .store
            .report_meta()
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect();
        let moved = e.plans.join("archive/old/r.html").display().to_string();
        assert!(paths.contains(&moved), "{paths:?}");
        assert!(paths.contains(&keep.display().to_string()));
        assert!(!paths.contains(&p.display().to_string()));
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn dry_run_changes_nothing() {
        let mut e = env("dry");
        e.cfg.inbox.keep_days = 10;
        write(&e, "old-group/SRE-1/ledger.jsonl", 40.0);
        write(&e, "loose.html", 40.0);
        let id = add(&e);
        e.store.resolve_item(id, "dismissed", "").unwrap();
        set_item(&e, id, e.now - 20.0 * DAY, Some(e.now - 20.0 * DAY), 0);
        let dry = run(&e.cfg, &e.store, e.now, true).unwrap();
        assert_eq!(dry.inbox_deleted, 1);
        assert_eq!(dry.archived.len(), 2);
        assert!(e.plans.join("old-group/SRE-1/ledger.jsonl").is_file());
        assert!(e.plans.join("loose.html").is_file());
        assert!(!e.plans.join("archive").exists());
        assert!(e.store.item(id).unwrap().is_some());
        let real = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert_eq!(names(&real), names(&dry));
        assert_eq!(real.inbox_deleted, 1);
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn zero_days_is_off() {
        let mut e = env("zero");
        e.cfg.reports.archive_after_days = 0;
        e.cfg.inbox.keep_days = 0;
        write(&e, "old/o.html", 400.0);
        let id = add(&e);
        e.store.resolve_item(id, "dismissed", "").unwrap();
        set_item(&e, id, e.now - 400.0 * DAY, Some(e.now - 400.0 * DAY), 0);
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(o.archived.is_empty());
        assert_eq!(o.inbox_deleted, 0);
        assert!(e.plans.join("old/o.html").is_file());
        assert!(e.store.item(id).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn prunes_resolved_inbox_items() {
        let mut e = env("inbox");
        e.cfg.reports.archive_after_days = 0;
        e.cfg.inbox.keep_days = 10;
        let old = e.now - 20.0 * DAY;
        let dismissed = add(&e);
        e.store.resolve_item(dismissed, "dismissed", "").unwrap();
        set_item(&e, dismissed, old, Some(old), 0);
        let delivered = add(&e);
        e.store.resolve_item(delivered, "answered", "y").unwrap();
        set_item(&e, delivered, old, Some(old), 1);
        let pending = add(&e);
        set_item(&e, pending, old, None, 0);
        let undelivered = add(&e);
        e.store.resolve_item(undelivered, "answered", "y").unwrap();
        set_item(&e, undelivered, old, Some(old), 0);
        let recent = add(&e);
        e.store.resolve_item(recent, "dismissed", "").unwrap();
        set_item(&e, recent, old, Some(e.now - 1.0 * DAY), 0);
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert_eq!(o.inbox_deleted, 2);
        assert!(e.store.item(dismissed).unwrap().is_none());
        assert!(e.store.item(delivered).unwrap().is_none());
        for id in [pending, undelivered, recent] {
            assert!(e.store.item(id).unwrap().is_some(), "{id}");
        }
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn daily_gate() {
        let e = env("gate");
        assert!(due(&e.dir, e.now));
        mark_ran(&e.dir, e.now).unwrap();
        assert!(!due(&e.dir, e.now + 3600.0));
        assert!(due(&e.dir, e.now + 90000.0));
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[test]
    fn archived_plans_are_hidden() {
        let e = env("hidden");
        write(&e, "old/SRE-1/ledger.jsonl", 40.0);
        write(&e, "old/r.html", 40.0);
        write(&e, "live/SRE-2/ledger.jsonl", 1.0);
        write(&e, "live/p.html", 1.0);
        assert_eq!(crate::ledger::list(&e.cfg).len(), 2);
        run(&e.cfg, &e.store, e.now, false).unwrap();
        let tickets: Vec<_> = crate::ledger::list(&e.cfg)
            .into_iter()
            .map(|r| r.ticket)
            .collect();
        assert_eq!(tickets, vec!["SRE-2"]);
        let reports = crate::reports::list(&e.cfg, &e.store).unwrap();
        assert_eq!(reports.len(), 1);
        let folders: Vec<_> = crate::reports::folders(&e.cfg)
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert!(!folders.contains(&"archive".to_string()), "{folders:?}");
        assert!(folders.contains(&"live".to_string()));
        let _ = std::fs::remove_dir_all(&e.dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_entries_are_skipped() {
        let e = env("symlink");
        let outside = e.dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("o.html"), "x").unwrap();
        age(&outside.join("o.html"), e.now, 90.0);
        std::os::unix::fs::symlink(&outside, e.plans.join("link")).unwrap();
        let o = run(&e.cfg, &e.store, e.now, false).unwrap();
        assert!(o.archived.is_empty());
        assert!(outside.join("o.html").is_file());
        assert!(e.plans.join("link").exists());
        let _ = std::fs::remove_dir_all(&e.dir);
    }
}
