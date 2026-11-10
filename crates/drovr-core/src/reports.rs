//! HTML reports in the plans folder (`~/.claude/plans` by default).
//!
//! The folder is the source of truth: every `.html` file in it, or in one
//! level of subfolders, is a report, whoever wrote it. A group's reports go in
//! `<plans>/<group>/`, a solo agent's in `<plans>/` itself.

use crate::config::Config;
use crate::engine::slug;
use crate::store::{ReportMeta, Store};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ReportFile {
    pub path: String,
    /// Subfolder under the plans folder; empty for the root.
    pub folder: String,
    pub title: String,
    pub modified: f64,
    /// Set when an agent published it through drovr.
    pub agent: String,
    pub summary: String,
    /// False only for files an agent published that the human hasn't opened.
    pub seen: bool,
}

/// A subfolder name of the plans folder: one path component, not hidden.
pub fn check_folder(folder: &str) -> Result<()> {
    let ok = !folder.is_empty()
        && !folder.starts_with('.')
        && !folder.contains(['/', '\\'])
        && Path::new(folder).components().count() == 1;
    if !ok {
        anyhow::bail!("{folder:?} isn't a folder name inside the plans folder");
    }
    Ok(())
}

/// The plans folder, or `folder` inside it when not empty.
fn folder_path(cfg: &Config, folder: &str) -> Result<PathBuf> {
    let mut dir = cfg.reports.path();
    if !folder.is_empty() {
        check_folder(folder)?;
        dir = dir.join(folder);
    }
    Ok(dir)
}

/// Write `html` to `folder` inside the plans folder (the root when empty)
/// under a name derived from `title`, never overwriting another report.
pub fn save(cfg: &Config, folder: &str, title: &str, html: &str) -> Result<PathBuf> {
    let dir = folder_path(cfg, folder)?;
    std::fs::create_dir_all(&dir)?;
    let base = slug(title);
    let mut path = dir.join(format!("{base}.html"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.html"));
        n += 1;
    }
    std::fs::write(&path, html)?;
    Ok(path)
}

/// Publish `agent`'s report into `folder` and mark it unread. A title that's
/// already published there, by any agent, is replaced: the newest copy is
/// overwritten and any older copies are deleted, so a revised plan never piles
/// up as `-2`, `-3`, …. Titles match ignoring case and punctuation, on the
/// title they were published under or the page's `<title>`. `keep_versions`
/// adds a new copy instead, for when the human asks to keep earlier versions.
#[allow(clippy::too_many_arguments)]
pub fn publish(
    cfg: &Config,
    store: &Store,
    folder: &str,
    agent: &str,
    title: &str,
    summary: &str,
    html: &str,
    keep_versions: bool,
) -> Result<PathBuf> {
    folder_path(cfg, folder)?;
    let root = cfg.reports.path();
    repair_paths(store, &report_files(&root))?;
    let folder = if folder.is_empty() {
        String::new()
    } else {
        disk_folder(&root, folder)
    };
    let folder = folder.as_str();
    let dir = folder_path(cfg, folder)?;
    let path = if keep_versions {
        save(cfg, folder, title, html)?
    } else {
        let keys: Vec<String> = [Some(title.to_string()), title_in(html)]
            .into_iter()
            .flatten()
            .map(|t| title_key(&t))
            .filter(|k| !k.is_empty())
            .collect();
        let mut copies: Vec<PathBuf> = store
            .report_meta()?
            .into_iter()
            // Only what agents published: never a file you wrote yourself.
            .filter(|m| !m.agent.is_empty())
            .map(|m| (PathBuf::from(&m.path), m.title))
            .filter(|(p, t)| {
                p.parent() == Some(dir.as_path())
                    && p.is_file()
                    && [t.clone(), title_of(p)]
                        .iter()
                        .any(|t| keys.contains(&title_key(t)))
            })
            .map(|(p, _)| p)
            .collect();
        copies.sort_by(|a, b| modified(b).total_cmp(&modified(a)));
        copies.dedup();
        match copies.split_first() {
            Some((newest, older)) => {
                std::fs::write(newest, html)?;
                for p in older {
                    std::fs::remove_file(p)?;
                    store.forget_report(&p.display().to_string())?;
                }
                newest.clone()
            }
            None => save(cfg, folder, title, html)?,
        }
    };
    store.publish_report(&path.display().to_string(), agent, title, summary)?;
    Ok(path)
}

/// A title for matching: lowercase words, ignoring punctuation and spacing.
fn title_key(title: &str) -> String {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn modified(p: &Path) -> f64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn is_html(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("html" | "htm")
    )
}

/// The page's `<title>`, or its file name. Only the head of the file is read:
/// the list refreshes every few seconds and reports can be large.
fn title_of(path: &Path) -> String {
    use std::io::Read;
    let mut bytes = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(16 * 1024).read_to_end(&mut bytes);
    }
    title_in(&String::from_utf8_lossy(&bytes)).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .replace(['-', '_'], " ")
    })
}

/// The `<title>` of an HTML page, if it has a non-empty one.
fn title_in(html: &str) -> Option<String> {
    // ASCII lowercasing keeps byte offsets, so they index `html` too.
    let lower = html.to_ascii_lowercase();
    let a = lower.find("<title>")? + "<title>".len();
    let len = lower[a..].find("</title>")?;
    let t = html[a..a + len].trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub fn list(cfg: &Config, store: &Store) -> Result<Vec<ReportFile>> {
    let files = report_files(&cfg.reports.path());
    repair_paths(store, &files)?;
    let meta: Vec<ReportMeta> = store.report_meta()?;
    let mut out: Vec<ReportFile> = files
        .into_iter()
        .map(|(path, folder)| {
            let key = path.display().to_string();
            let m = meta.iter().find(|m| m.path == key);
            ReportFile {
                title: title_of(&path),
                folder,
                modified: modified(&path),
                agent: m.map(|m| m.agent.clone()).unwrap_or_default(),
                summary: m.map(|m| m.summary.clone()).unwrap_or_default(),
                seen: m.map(|m| m.seen).unwrap_or(true),
                path: key,
            }
        })
        .collect();
    out.sort_by(|a, b| b.modified.total_cmp(&a.modified));
    Ok(out)
}

/// The HTML files the Reports list shows, each with its subfolder (empty for
/// the plans folder itself).
fn report_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = vec![];
    let mut visit = |dir: &Path, folder: &str| {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_file() && is_html(&path) {
                out.push((path, folder.to_string()));
            }
        }
    };
    visit(root, "");
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name != ARCHIVE {
                visit(&entry.path(), &name);
            }
        }
    }
    out
}

/// Archived reports go in an `archive/` folder beside them, out of the list.
pub const ARCHIVE: &str = "archive";

/// Move a listed report into the `archive/` folder beside it. Returns where
/// it went.
pub fn archive(cfg: &Config, store: &Store, path: &str) -> Result<PathBuf> {
    let from = PathBuf::from(path);
    if !report_files(&cfg.reports.path())
        .iter()
        .any(|(p, _)| p == &from)
    {
        anyhow::bail!("{path} isn't a report drovr lists");
    }
    let dir = from.parent().unwrap_or(Path::new(".")).join(ARCHIVE);
    std::fs::create_dir_all(&dir)?;
    let mut to = dir.join(from.file_name().unwrap_or_default());
    let stem = to
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut n = 2;
    while to.exists() {
        to = dir.join(format!("{stem}-{n}.html"));
        n += 1;
    }
    std::fs::rename(&from, &to)?;
    store.forget_report(path)?;
    Ok(to)
}

/// `folder` as it's spelled on disk. A disk that ignores case treats `sre-1`
/// and `SRE-1` as one folder, but paths compare as text, so a report has to be
/// recorded under the spelling the list reads back.
fn disk_folder(root: &Path, folder: &str) -> String {
    let Ok(wanted) = root.join(folder).canonicalize() else {
        return folder.to_string();
    };
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .find(|e| e.path().canonicalize().is_ok_and(|p| p == wanted))
        .map_or(folder.to_string(), |e| {
            e.file_name().to_string_lossy().to_string()
        })
}

/// Move rows recorded under another spelling of a listed file's path onto the
/// listed path, so each report has one row. Rows for missing files stay as
/// they are.
fn repair_paths(store: &Store, files: &[(PathBuf, String)]) -> Result<()> {
    let listed: HashSet<String> = files.iter().map(|(p, _)| p.display().to_string()).collect();
    let moved: Vec<(String, PathBuf)> = store
        .report_meta()?
        .into_iter()
        .filter(|m| !listed.contains(&m.path))
        .filter_map(|m| Some((Path::new(&m.path).canonicalize().ok()?, m.path)))
        .map(|(real, path)| (path, real))
        .collect();
    if moved.is_empty() {
        return Ok(());
    }
    let by_real: HashMap<PathBuf, String> = files
        .iter()
        .filter_map(|(p, _)| Some((p.canonicalize().ok()?, p.display().to_string())))
        .collect();
    for (path, real) in moved {
        if let Some(to) = by_real.get(&real) {
            store.move_report(&path, to)?;
        }
    }
    Ok(())
}

/// How many reports are unread. A file that's gone can't be opened, so it
/// doesn't count, and two spellings of one path count once.
pub fn unseen(store: &Store) -> Result<i64> {
    let unread: HashSet<PathBuf> = store
        .report_meta()?
        .into_iter()
        .filter(|m| !m.seen)
        .filter_map(|m| Path::new(&m.path).canonicalize().ok())
        .filter(|p| p.is_file())
        .collect();
    Ok(unread.len() as i64)
}

/// A subfolder of the plans folder and the files in it that can serve as
/// prompts (Markdown and text).
#[derive(Debug, Clone, Serialize)]
pub struct PlansFolder {
    /// Empty for the plans folder itself.
    pub name: String,
    pub prompts: Vec<String>,
}

fn prompt_files(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| {
            matches!(
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(str::to_lowercase)
                    .as_deref(),
                Some("md" | "txt")
            )
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();
    files.sort();
    files
}

/// The plans folder's root and its subfolders, each with its prompt files.
pub fn folders(cfg: &Config) -> Vec<PlansFolder> {
    let root = cfg.reports.path();
    let mut out = vec![PlansFolder {
        name: String::new(),
        prompts: prompt_files(&root),
    }];
    let mut subs: Vec<_> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        // A solo ticket's record and the retros sit here too; they're not
        // plans folders.
        .filter(|e| {
            e.path().is_dir() && !crate::ledger::is_record(&e.path()) && e.file_name() != "retros"
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();
    subs.sort_by_key(|n| n.to_lowercase());
    for name in subs {
        out.push(PlansFolder {
            prompts: prompt_files(&root.join(&name)),
            name,
        });
    }
    out
}

/// Read a prompt file from `folder` (the root when empty) in the plans folder.
pub fn read_prompt(cfg: &Config, folder: &str, file: &str) -> Result<String> {
    if !folder.is_empty() {
        check_folder(folder)?;
    }
    check_folder(file)?;
    let path = cfg.reports.path().join(folder).join(file);
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plans folder holding a ticket's `SRE-1` folder. None where the disk
    /// tells `sre-1` from `SRE-1`, since the two are then different folders.
    fn differs_by_case(name: &str) -> Option<(PathBuf, Config, Store)> {
        let dir =
            std::env::temp_dir().join(format!("drovr-reports-case-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("SRE-1")).unwrap();
        let dir = dir.canonicalize().unwrap();
        if !dir.join("sre-1").exists() {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        let store = Store::open(&dir.join("t.db")).unwrap();
        Some((dir, cfg, store))
    }

    #[test]
    fn publishing_to_a_folder_differing_by_case_stays_unread() {
        let Some((dir, cfg, store)) = differs_by_case("publish") else {
            return;
        };
        let path = publish(
            &cfg, &store, "sre-1", "lead", "Plan", "why", "<p>x</p>", false,
        )
        .unwrap();
        assert!(path.ends_with("SRE-1/plan.html"), "{}", path.display());
        let files = list(&cfg, &store).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(
            (
                f.folder.as_str(),
                f.agent.as_str(),
                f.summary.as_str(),
                f.seen
            ),
            ("SRE-1", "lead", "why", false)
        );
        assert_eq!(unseen(&store).unwrap(), 1);
        store.mark_report_seen(&f.path).unwrap();
        assert_eq!(unseen(&store).unwrap(), 0);
        assert!(list(&cfg, &store).unwrap()[0].seen);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_recorded_under_another_case_are_repaired() {
        let Some((dir, cfg, store)) = differs_by_case("repair") else {
            return;
        };
        std::fs::write(dir.join("SRE-1/plan.html"), "<p>x</p>").unwrap();
        let lower = dir.join("sre-1/plan.html").display().to_string();
        let upper = dir.join("SRE-1/plan.html").display().to_string();
        store.publish_report(&lower, "lead", "Plan", "why").unwrap();
        // Opening it in the old version added a second row.
        store.mark_report_seen(&upper).unwrap();

        let files = list(&cfg, &store).unwrap();
        assert_eq!(
            (
                files[0].agent.as_str(),
                files[0].summary.as_str(),
                files[0].seen
            ),
            ("lead", "why", false)
        );
        let rows = store.report_meta().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].path, upper);
        assert_eq!(unseen(&store).unwrap(), 1);
        store.mark_report_seen(&files[0].path).unwrap();
        assert_eq!(unseen(&store).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn republishing_replaces_a_copy_recorded_under_another_case() {
        let Some((dir, cfg, store)) = differs_by_case("republish") else {
            return;
        };
        std::fs::write(dir.join("SRE-1/plan.html"), "<p>one</p>").unwrap();
        let lower = dir.join("sre-1/plan.html").display().to_string();
        store.publish_report(&lower, "lead", "Plan", "").unwrap();

        publish(
            &cfg,
            &store,
            "sre-1",
            "lead",
            "Plan",
            "",
            "<p>two</p>",
            false,
        )
        .unwrap();
        let files = list(&cfg, &store).unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(
            std::fs::read_to_string(&files[0].path).unwrap(),
            "<p>two</p>"
        );
        assert_eq!(unseen(&store).unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_report_is_not_counted_as_unread() {
        let dir = std::env::temp_dir().join(format!("drovr-reports-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let gone = publish(&cfg, &store, "", "lead", "Gone", "", "<p>x</p>", false).unwrap();
        publish(&cfg, &store, "", "lead", "Here", "", "<p>x</p>", false).unwrap();
        assert_eq!(unseen(&store).unwrap(), 2);
        std::fs::remove_file(&gone).unwrap();
        assert_eq!(unseen(&store).unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saves_into_group_folder_and_lists() {
        let dir = std::env::temp_dir().join(format!("drovr-reports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();

        let a = save(
            &cfg,
            "pay-412",
            "Refund design",
            "<title>Refund design</title>",
        )
        .unwrap();
        let b = save(&cfg, "pay-412", "Refund design", "<p>v2</p>").unwrap();
        let c = save(&cfg, "", "Flaky CI", "<p>x</p>").unwrap();
        std::fs::write(dir.join("notes.md"), "not html").unwrap();
        assert!(a.ends_with("pay-412/refund-design.html"));
        assert!(b.ends_with("pay-412/refund-design-2.html"));
        assert_eq!(c.parent().unwrap(), dir);

        store
            .publish_report(
                &a.display().to_string(),
                "pay-412-lead",
                "Refund design",
                "the plan",
            )
            .unwrap();
        let files = list(&cfg, &store).unwrap();
        assert_eq!(files.len(), 3);
        let fa = files
            .iter()
            .find(|f| f.path == a.display().to_string())
            .unwrap();
        assert_eq!(
            (
                fa.folder.as_str(),
                fa.title.as_str(),
                fa.agent.as_str(),
                fa.seen
            ),
            ("pay-412", "Refund design", "pay-412-lead", false)
        );
        let fc = files
            .iter()
            .find(|f| f.path == c.display().to_string())
            .unwrap();
        assert_eq!(
            (fc.folder.as_str(), fc.title.as_str(), fc.seen),
            ("", "flaky ci", true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn republishing_a_title_replaces_it() {
        let dir = std::env::temp_dir().join(format!("drovr-republish-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let publish = |agent: &str, title: &str, html: &str| {
            publish(&cfg, &store, "sre-1", agent, title, "s", html, false).unwrap()
        };
        let read = |p: &PathBuf| std::fs::read_to_string(p).unwrap();

        let a = publish("lead", "Cutover runbook", "<p>v1</p>");
        store.mark_report_seen(&a.display().to_string()).unwrap();
        // Same title: the same file, unread again.
        assert_eq!(publish("lead", "Cutover runbook", "<p>v2</p>"), a);
        assert_eq!(read(&a), "<p>v2</p>");
        // Whichever agent publishes it, and however the title is cased or punctuated.
        assert_eq!(publish("worker", "Cutover runbook", "<p>w</p>"), a);
        assert_eq!(publish("lead-2", "cutover  Runbook!", "<p>v3</p>"), a);
        assert_eq!(read(&a), "<p>v3</p>");
        // A title argument that drifts still matches on the page's <title>.
        assert_eq!(
            publish(
                "lead",
                "Cutover runbook (revised)",
                "<title>Cutover runbook</title>4"
            ),
            a
        );
        // Another title is another report, even one that shares a long prefix.
        let c = publish("lead", "Cutover runbook 2", "<p>c</p>");
        assert_ne!(c, a);
        let long1 = publish("lead", "Payments API migration plan phase one", "<p>1</p>");
        let long2 = publish("lead", "Payments API migration plan phase two", "<p>2</p>");
        assert_ne!(long1, long2);
        // A file you wrote yourself with the same <title> is never touched.
        let mine = dir.join("sre-1/mine.html");
        std::fs::write(&mine, "<title>Cutover runbook</title>mine").unwrap();
        store.mark_report_seen(&mine.display().to_string()).unwrap();
        assert_eq!(publish("lead", "Cutover runbook", "<p>v5</p>"), a);
        assert_eq!(read(&mine), "<title>Cutover runbook</title>mine");

        let files = list(&cfg, &store).unwrap();
        assert_eq!(files.len(), 5);
        assert!(
            !files
                .iter()
                .find(|f| f.path == a.display().to_string())
                .unwrap()
                .seen
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn republishing_removes_older_copies() {
        let dir = std::env::temp_dir().join(format!("drovr-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();

        // Copies left over from before republishing replaced reports: one with
        // no stored title, which goes by its <title>.
        let old1 = save(&cfg, "sre-1", "Old plan", "<title>Old plan</title>1").unwrap();
        let old2 = save(&cfg, "sre-1", "Old plan", "<p>2</p>").unwrap();
        let old3 = save(&cfg, "sre-1", "Old plan", "<p>3</p>").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&old2)
            .unwrap()
            .set_modified(later)
            .unwrap();
        store
            .publish_report(&old1.display().to_string(), "lead", "", "s")
            .unwrap();
        for p in [&old2, &old3] {
            store
                .publish_report(&p.display().to_string(), "worker", "Old plan", "s")
                .unwrap();
        }
        // The same title in another folder is another report.
        let elsewhere =
            publish(&cfg, &store, "sre-2", "lead", "Old plan", "s", "x", false).unwrap();

        let kept = publish(&cfg, &store, "sre-1", "lead", "Old plan", "s", "new", false).unwrap();
        assert_eq!(kept, old2, "the newest copy is the one kept");
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "new");
        assert!(!old1.exists() && !old3.exists());
        assert!(elsewhere.exists());
        let paths: Vec<String> = store
            .report_meta()
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert!(!paths.contains(&old1.display().to_string()));
        assert!(!paths.contains(&old3.display().to_string()));
        assert_eq!(list(&cfg, &store).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keep_versions_adds_a_copy() {
        let dir = std::env::temp_dir().join(format!("drovr-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();

        let a = publish(&cfg, &store, "", "solo", "Design", "s", "v1", false).unwrap();
        let b = publish(&cfg, &store, "", "solo", "Design", "s", "v2", true).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v1");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "v2");
        // Without it, the next publish replaces the newest and clears the rest.
        std::fs::File::options()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            publish(&cfg, &store, "", "solo", "Design", "s", "v3", false).unwrap(),
            b
        );
        assert!(!a.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archiving_takes_a_report_out_of_the_list() {
        let dir = std::env::temp_dir().join(format!("drovr-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.reports.dir = dir.join("plans").display().to_string();
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let p = publish(&cfg, &store, "g", "lead", "Plan", "s", "<p>x</p>", false).unwrap();
        let to = archive(&cfg, &store, &p.display().to_string()).unwrap();
        assert!(to.ends_with("plans/g/archive/plan.html"));
        assert!(list(&cfg, &store).unwrap().is_empty());
        assert!(archive(&cfg, &store, "/etc/hosts").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_folders_and_reads_prompts() {
        let dir = std::env::temp_dir().join(format!("drovr-plans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Pay API")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(
            dir.join("Pay API/coordinator.md"),
            "Coordinate the pay-api work",
        )
        .unwrap();
        std::fs::write(dir.join("Pay API/design.html"), "<p>x</p>").unwrap();
        std::fs::write(dir.join("root-plan.md"), "root").unwrap();
        let mut cfg = Config::default();
        cfg.reports.dir = dir.display().to_string();

        let f = folders(&cfg);
        assert_eq!(f.len(), 2);
        assert_eq!(
            (f[0].name.as_str(), f[0].prompts.clone()),
            ("", vec!["root-plan.md".to_string()])
        );
        assert_eq!(
            (f[1].name.as_str(), f[1].prompts.clone()),
            ("Pay API", vec!["coordinator.md".to_string()])
        );
        assert_eq!(
            read_prompt(&cfg, "Pay API", "coordinator.md").unwrap(),
            "Coordinate the pay-api work"
        );
        assert!(read_prompt(&cfg, "..", "x.md").is_err());
        assert!(read_prompt(&cfg, "Pay API", "../../etc/passwd").is_err());
        assert!(save(&cfg, "../escape", "t", "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn odd_titles_dont_panic() {
        let dir = std::env::temp_dir().join(format!("drovr-titles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let title = |name: &str, html: &str| {
            let p = dir.join(name);
            std::fs::write(&p, html).unwrap();
            title_of(&p)
        };
        // A closing tag before the opening one used to slice backwards.
        assert_eq!(
            title("early-close.html", "<!-- </title> --><title>Real</title>"),
            "Real"
        );
        // Characters whose lowercase is longer used to shift the offsets.
        assert_eq!(
            title("unicode.html", "<p>İİİİ</p><TITLE>Ünïcode</TITLE>"),
            "Ünïcode"
        );
        assert_eq!(title("no-close.html", "<title>dangling"), "no close");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
