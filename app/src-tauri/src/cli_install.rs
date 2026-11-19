//! Install Command Line Tool: link `/usr/local/bin/drovr` to the CLI bundled
//! in the app, so it updates with the app. The decisions are pure and take
//! their paths as parameters, so tests never touch the real `/usr/local/bin`.

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const TARGET: &str = "/usr/local/bin/drovr";

#[derive(Debug, PartialEq)]
pub enum Plan {
    Create,
    AlreadyInstalled,
    Replace { old: PathBuf },
    Refuse,
}

#[derive(Debug, PartialEq)]
pub enum Admin {
    Done,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CliStatus {
    pub installed: bool,
    pub target: String,
    pub points_to: Option<String>,
    pub on_path: Option<String>,
    pub shadowed: bool,
}

pub fn decide(target: &Path, src: &Path) -> Plan {
    match std::fs::symlink_metadata(target) {
        Err(_) => Plan::Create,
        Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(target) {
            Ok(to) if to == src => Plan::AlreadyInstalled,
            Ok(old) => Plan::Replace { old },
            Err(_) => Plan::Refuse,
        },
        Ok(_) => Plan::Refuse,
    }
}

/// Quote for a POSIX shell: plain paths as they are, anything else in single
/// quotes with each `'` written as `'\''`.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-' | '+'));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Escape for the inside of an AppleScript double-quoted string.
pub fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The shell command that links `target` to `src`, already escaped for
/// `do shell script "<here>"`.
pub fn admin_script(src: &Path, target: &Path) -> String {
    let dir = target.parent().unwrap_or_else(|| Path::new("/"));
    let cmd = format!(
        "mkdir -p {} && ln -sfn {} {}",
        shell_quote(&dir.to_string_lossy()),
        shell_quote(&src.to_string_lossy()),
        shell_quote(&target.to_string_lossy()),
    );
    applescript_escape(&cmd)
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// What `name` resolves to on `path_var`: the first executable file.
pub fn resolve_on_path(path_var: &str, name: &str) -> Option<PathBuf> {
    path_var
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| is_executable_file(p))
}

pub fn status_in(target: &Path, src: Option<&Path>, path_var: &str) -> CliStatus {
    let link = std::fs::read_link(target).ok();
    let installed = match (&link, src) {
        (Some(to), Some(src)) => to == src && src.is_file(),
        _ => false,
    };
    let on_path = resolve_on_path(path_var, "drovr");
    let shadowed = on_path.as_ref().is_some_and(|p| {
        p != target
            && match (p.canonicalize(), target.canonicalize()) {
                (Ok(a), Ok(b)) => a != b,
                _ => true,
            }
    });
    CliStatus {
        installed,
        target: target.display().to_string(),
        points_to: link.map(|p| p.display().to_string()),
        on_path: on_path.map(|p| p.display().to_string()),
        shadowed,
    }
}

pub fn install_at(
    target: &Path,
    src: &Path,
    elevate: impl FnOnce(&str) -> Admin,
) -> Result<String, String> {
    let plan = decide(target, src);
    let done = match &plan {
        Plan::AlreadyInstalled => return Ok("Already installed.".into()),
        Plan::Refuse => {
            return Err(format!(
                "{} exists and isn't a link; remove it first",
                target.display()
            ))
        }
        Plan::Create => format!("Installed: {} → {}", target.display(), src.display()),
        Plan::Replace { old } => format!("Replaced the link to {}.", old.display()),
    };
    match link(target, src, &plan) {
        Ok(()) => Ok(done),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            match elevate(&admin_script(src, target)) {
                Admin::Done => Ok(done),
                Admin::Cancelled => Ok("Cancelled.".into()),
                Admin::Failed(why) => Err(format!("Couldn't link {}: {why}", target.display())),
            }
        }
        Err(e) => Err(format!("Couldn't link {}: {e}", target.display())),
    }
}

fn link(target: &Path, src: &Path, plan: &Plan) -> std::io::Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if matches!(plan, Plan::Replace { .. }) {
        std::fs::remove_file(target)?;
    }
    std::os::unix::fs::symlink(src, target)
}

fn osascript(script: &str) -> Admin {
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!(
            "do shell script \"{script}\" with administrator privileges"
        ))
        .output();
    match out {
        Err(e) => Admin::Failed(e.to_string()),
        Ok(o) if o.status.success() => Admin::Done,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            if err.contains("-128") || err.to_lowercase().contains("user canceled") {
                Admin::Cancelled
            } else {
                Admin::Failed(err.trim().to_string())
            }
        }
    }
}

/// Why the app's own location would make a link that stops working: run from
/// the mounted .dmg, or from the random path macOS moves a quarantined app to.
pub fn unstable_location(src: &Path) -> Option<&'static str> {
    let s = src.to_string_lossy();
    if s.contains("/AppTranslocation/") {
        Some("macOS is running drovr from a temporary copy; move drovr.app to Applications, open it from there, then try again")
    } else if s.starts_with("/Volumes/") {
        Some("drovr is running from the disk image; drag it to Applications, open it from there, then try again")
    } else {
        None
    }
}

pub fn cli_source() -> Result<PathBuf, String> {
    let src = drovr_core::engine::locate_cli();
    if !src.is_file() {
        return Err("drovr's CLI isn't in this app bundle".into());
    }
    match unstable_location(&src) {
        Some(why) => Err(why.into()),
        None => Ok(src),
    }
}

pub fn status() -> CliStatus {
    let src = cli_source().ok();
    let path = std::env::var("PATH").unwrap_or_default();
    status_in(Path::new(TARGET), src.as_deref(), &path)
}

pub fn install() -> Result<String, String> {
    let src = cli_source()?;
    install_at(Path::new(TARGET), &src, osascript)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "drovr-cli-install-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        fn file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_target_is_created() {
        let d = Dir::new();
        let src = d.file("src");
        assert_eq!(decide(&d.0.join("drovr"), &src), Plan::Create);
    }

    #[test]
    fn link_to_source_is_already_installed() {
        let d = Dir::new();
        let src = d.file("src");
        let t = d.0.join("drovr");
        symlink(&src, &t).unwrap();
        assert_eq!(decide(&t, &src), Plan::AlreadyInstalled);
    }

    #[test]
    fn link_elsewhere_is_replaced() {
        let d = Dir::new();
        let src = d.file("src");
        let old = d.file("old");
        let t = d.0.join("drovr");
        symlink(&old, &t).unwrap();
        assert_eq!(decide(&t, &src), Plan::Replace { old });
    }

    #[test]
    fn dangling_link_is_replaced() {
        let d = Dir::new();
        let src = d.file("src");
        let t = d.0.join("drovr");
        symlink(d.0.join("gone"), &t).unwrap();
        assert_eq!(
            decide(&t, &src),
            Plan::Replace {
                old: d.0.join("gone")
            }
        );
    }

    #[test]
    fn real_file_or_directory_is_refused() {
        let d = Dir::new();
        let src = d.file("src");
        let f = d.file("drovr");
        assert_eq!(decide(&f, &src), Plan::Refuse);
        let dir = d.0.join("sub");
        fs::create_dir(&dir).unwrap();
        assert_eq!(decide(&dir, &src), Plan::Refuse);
    }

    #[test]
    fn temporary_app_locations_are_refused() {
        for bad in [
            "/private/var/folders/x/T/AppTranslocation/ABC/d/drovr.app/Contents/MacOS/drovr",
            "/Volumes/drovr/drovr.app/Contents/MacOS/drovr",
        ] {
            assert!(super::unstable_location(Path::new(bad)).is_some(), "{bad}");
        }
        let good = Path::new("/Applications/drovr.app/Contents/MacOS/drovr");
        assert!(super::unstable_location(good).is_none());
    }

    #[test]
    fn quoting_survives_space_single_and_double_quote() {
        let p = r#"/Applications/my "app"/it's here/drovr"#;
        assert_eq!(
            shell_quote(p),
            r#"'/Applications/my "app"/it'\''s here/drovr'"#
        );
        assert_eq!(applescript_escape(r#"a\b"c"#), r#"a\\b\"c"#);
        let script = admin_script(Path::new(p), Path::new("/usr/local/bin/drovr"));
        assert_eq!(
            script,
            r#"mkdir -p /usr/local/bin && ln -sfn '/Applications/my \"app\"/it'\\''s here/drovr' /usr/local/bin/drovr"#
        );
    }

    #[test]
    fn path_resolution_takes_first_executable() {
        let d = Dir::new();
        let a = d.0.join("a");
        let b = d.0.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("drovr"), "not executable").unwrap();
        let hit = b.join("drovr");
        fs::write(&hit, "x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hit, fs::Permissions::from_mode(0o755)).unwrap();
        let var = format!("{}::{}", a.display(), b.display());
        assert_eq!(resolve_on_path(&var, "drovr"), Some(hit));
        assert_eq!(resolve_on_path("", "drovr"), None);
    }

    #[test]
    fn status_reports_installed_and_shadowed() {
        let d = Dir::new();
        let src = d.file("src");
        let bin = d.0.join("bin");
        let other = d.0.join("other");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&other).unwrap();
        let target = bin.join("drovr");
        symlink(&src, &target).unwrap();
        let shadow = other.join("drovr");
        fs::copy(&src, &shadow).unwrap();

        let ok = status_in(&target, Some(&src), &bin.display().to_string());
        assert!(ok.installed && !ok.shadowed);
        assert_eq!(ok.points_to, Some(src.display().to_string()));
        assert_eq!(ok.on_path, Some(target.display().to_string()));

        let var = format!("{}:{}", other.display(), bin.display());
        let sh = status_in(&target, Some(&src), &var);
        assert!(sh.installed && sh.shadowed);
        assert_eq!(sh.on_path, Some(shadow.display().to_string()));

        let none = status_in(&bin.join("nope"), Some(&src), "");
        assert!(!none.installed && !none.shadowed && none.on_path.is_none());
    }

    #[test]
    fn install_creates_replaces_and_noops() {
        let d = Dir::new();
        let src = d.file("src");
        let t = d.0.join("nested/bin/drovr");
        let never = |_: &str| -> Admin { panic!("no elevation needed") };
        let msg = install_at(&t, &src, never).unwrap();
        assert_eq!(
            msg,
            format!("Installed: {} → {}", t.display(), src.display())
        );
        assert_eq!(fs::read_link(&t).unwrap(), src);
        assert_eq!(install_at(&t, &src, never).unwrap(), "Already installed.");

        let src2 = d.file("src2");
        let msg = install_at(&t, &src2, never).unwrap();
        assert_eq!(msg, format!("Replaced the link to {}.", src.display()));
        assert_eq!(fs::read_link(&t).unwrap(), src2);
    }

    #[test]
    fn install_refuses_real_file() {
        let d = Dir::new();
        let src = d.file("src");
        let t = d.file("drovr");
        let e = install_at(&t, &src, |_| panic!("no")).unwrap_err();
        assert!(
            e.contains("exists and isn't a link; remove it first"),
            "{e}"
        );
        assert!(!fs::symlink_metadata(&t).unwrap().file_type().is_symlink());
    }

    #[test]
    fn permission_denied_elevates() {
        let d = Dir::new();
        let src = d.file("src");
        let ro = d.0.join("ro");
        fs::create_dir(&ro).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        let t = ro.join("drovr");
        if fs::write(ro.join("probe"), "").is_ok() {
            // Running as root: can't provoke PermissionDenied.
            fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let mut got = String::new();
        let r = install_at(&t, &src, |s| {
            got = s.to_string();
            Admin::Done
        });
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.unwrap().starts_with("Installed: "));
        assert_eq!(got, admin_script(&src, &t));

        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        let c = install_at(&t, &src, |_| Admin::Cancelled);
        let f = install_at(&t, &src, |_| Admin::Failed("boom".into()));
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(c.unwrap(), "Cancelled.");
        assert!(f.unwrap_err().contains("boom"));
    }
}
