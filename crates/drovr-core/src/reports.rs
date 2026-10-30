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

/// Publish `agent`'s report into `folder` and mark it unread. Publishing a
/// title the agent already published there replaces its newest copy, so a
/// revised plan updates in place instead of piling up as `-2`, `-3`, ….
pub fn publish(
    cfg: &Config,
    store: &Store,
    folder: &str,
    agent: &str,
    title: &str,
    summary: &str,
    html: &str,
) -> Result<PathBuf> {
    let dir = folder_path(cfg, folder)?;
    let previous = store
        .report_meta()?
        .into_iter()
        .filter(|m| m.agent == agent)
        .map(|m| (PathBuf::from(&m.path), m.title))
        .filter(|(p, t)| {
            p.parent() == Some(dir.as_path())
                && p.is_file()
                // Reports from before titles were kept go by their <title>.
                && if t.is_empty() { title_of(p) == title } else { t == title }
        })
        .max_by(|(a, _), (b, _)| modified(a).total_cmp(&modified(b)));
    let path = match previous {
        Some((path, _)) => {
            std::fs::write(&path, html)?;
            path
        }
        None => save(cfg, folder, title, html)?,
    };
    store.publish_report(&path.display().to_string(), agent, title, summary)?;
    Ok(path)
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
    let head = String::from_utf8_lossy(&bytes);
    // ASCII lowercasing keeps byte offsets, so they index `head` too.
    let lower = head.to_ascii_lowercase();
    if let Some(a) = lower.find("<title>").map(|a| a + "<title>".len()) {
        if let Some(len) = lower[a..].find("</title>") {
            let t = head[a..a + len].trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .replace(['-', '_'], " ")
}

pub fn list(cfg: &Config, store: &Store) -> Result<Vec<ReportFile>> {
    let root = cfg.reports.path();
    let meta: Vec<ReportMeta> = store.report_meta()?;
    let mut out = vec![];
    let mut visit = |dir: &Path, folder: &str| {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if !path.is_file() || !is_html(&path) {
                continue;
            }
            let modified = modified(&path);
            let key = path.display().to_string();
            let m = meta.iter().find(|m| m.path == key);
            out.push(ReportFile {
                title: title_of(&path),
                folder: folder.to_string(),
                modified,
                agent: m.map(|m| m.agent.clone()).unwrap_or_default(),
                summary: m.map(|m| m.summary.clone()).unwrap_or_default(),
                seen: m.map(|m| m.seen).unwrap_or(true),
                path: key,
            });
        }
    };
    visit(&root, "");
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') {
                visit(&entry.path(), &name);
            }
        }
    }
    out.sort_by(|a, b| b.modified.total_cmp(&a.modified));
    Ok(out)
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
            publish(&cfg, &store, "sre-1", agent, title, "s", html).unwrap()
        };

        let a = publish("lead", "Cutover runbook", "<p>v1</p>");
        store.mark_report_seen(&a.display().to_string()).unwrap();
        // Same agent, same title: the same file, unread again.
        assert_eq!(publish("lead", "Cutover runbook", "<p>v2</p>"), a);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "<p>v2</p>");
        // Another agent's report of the same title, and another title, are new files.
        let b = publish("worker", "Cutover runbook", "<p>w</p>");
        let c = publish("lead", "Cutover runbook 2", "<p>c</p>");
        assert!(b.ends_with("sre-1/cutover-runbook-2.html"));
        assert!(c.ends_with("sre-1/cutover-runbook-2-2.html"));
        assert_eq!(publish("lead", "Cutover runbook", "<p>v3</p>"), a);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "<p>w</p>");
        let files = list(&cfg, &store).unwrap();
        assert_eq!(files.len(), 3);
        assert!(
            !files
                .iter()
                .find(|f| f.path == a.display().to_string())
                .unwrap()
                .seen
        );

        // Copies published before titles were kept match on their <title>,
        // and the newest one is replaced.
        let old1 = save(&cfg, "sre-1", "Old plan", "<title>Old plan</title>1").unwrap();
        let old2 = save(&cfg, "sre-1", "Old plan", "<title>Old plan</title>2").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&old2)
            .unwrap()
            .set_modified(later)
            .unwrap();
        for p in [&old1, &old2] {
            store
                .publish_report(&p.display().to_string(), "lead", "", "s")
                .unwrap();
        }
        assert_eq!(
            publish("lead", "Old plan", "<title>Old plan</title>3"),
            old2
        );
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
