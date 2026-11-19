//! PATH for an app launched from the Dock.
//!
//! macOS gives such an app `/usr/bin:/bin:/usr/sbin:/sbin`. We ask the user's
//! shell for its PATH, falling back to the last one that worked.

use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MARKER: &str = "__DROVR_PATH__";
const SAVED_FILE: &str = "login-path";
pub const INTERACTIVE_SECS: u64 = 20;
pub const LOGIN_SECS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Info {
    pub source: String,
    pub ms: u64,
    pub error: Option<String>,
    pub path: String,
}

#[derive(Debug, PartialEq)]
pub struct Resolved {
    pub source: &'static str,
    pub path: Option<String>,
    pub error: Option<String>,
}

pub fn parse_marker(stdout: &str) -> Option<String> {
    stdout
        .split(MARKER)
        .nth(1)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// Keep `path` as is, then append each existing folder of `common` it lacks.
pub fn merge_common(path: &str, common: &[PathBuf], is_dir: impl Fn(&Path) -> bool) -> String {
    let mut out: Vec<String> = Vec::new();
    for entry in path.split(':').filter(|e| !e.is_empty()) {
        if !out.iter().any(|o| o == entry) {
            out.push(entry.to_string());
        }
    }
    for dir in common {
        let entry = dir.to_string_lossy();
        if !out.iter().any(|o| *o == entry) && is_dir(dir) {
            out.push(entry.into_owned());
        }
    }
    out.join(":")
}

pub fn common_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join(".opencode/bin"),
        home.join(".cargo/bin"),
    ]
}

pub fn read_saved(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(SAVED_FILE)).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub fn write_saved(dir: &Path, path: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!("{SAVED_FILE}.tmp{}", std::process::id()));
    std::fs::write(&tmp, path)?;
    std::fs::rename(&tmp, dir.join(SAVED_FILE)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Interactive login shell first, then a non-interactive login shell, then
/// the PATH saved from an earlier start, then nothing (macOS's default).
pub fn resolve(
    interactive: impl FnOnce() -> Result<String, String>,
    login: impl FnOnce() -> Result<String, String>,
    saved: Option<String>,
) -> Resolved {
    let first = match interactive() {
        Ok(path) => {
            return Resolved {
                source: "interactive",
                path: Some(path),
                error: None,
            };
        }
        Err(e) => format!("interactive shell: {e}"),
    };
    let second = match login() {
        Ok(path) => {
            return Resolved {
                source: "login",
                path: Some(path),
                error: Some(first),
            };
        }
        Err(e) => format!("login shell: {e}"),
    };
    let error = Some(format!("{first}; {second}"));
    match saved {
        Some(path) => Resolved {
            source: "saved",
            path: Some(path),
            error,
        },
        None => Resolved {
            source: "system",
            path: None,
            error,
        },
    }
}

/// Run `cmd` with no stdin and no controlling terminal (its own session), so
/// anything that wants to prompt fails at once instead of waiting for input.
/// On `timeout` it sends SIGTERM to the whole group and SIGKILL 3s later, so
/// shell EXIT traps (pyenv's lock file) run.
fn run_detached(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("DISABLE_AUTO_UPDATE", "true");
    // SAFETY: setsid is async-signal-safe; it makes the child a session and
    // group leader, so /dev/tty can't be opened and killpg(pid) reaches all of it.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start: {e}"))?;
    let mut pipe = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = pipe.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    });
    let start = Instant::now();
    let mut nap = Duration::from_millis(1);
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if start.elapsed() > timeout {
            timed_out = true;
            drovr_core::shell::terminate_group(&mut child, drovr_core::shell::KILL_GRACE);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(50));
    }
    let out = reader.join().unwrap_or_default();
    if timed_out {
        return Err(format!("no answer in {}s", timeout.as_secs()));
    }
    parse_marker(&out).ok_or_else(|| "no PATH in its output".to_string())
}

fn probe(shell: &str, flags: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(shell);
    cmd.args(flags)
        .arg("-c")
        .arg(format!("printf '{MARKER}%s{MARKER}' \"$PATH\""));
    run_detached(cmd, timeout)
}

static INFO: OnceLock<Info> = OnceLock::new();
static READY: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn mark_ready() {
    *READY.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
    READY.1.notify_all();
}

/// Block until PATH is settled, for at most `max`. Threads that spawn `gh`,
/// `claude` or `opencode` call this before their first run.
pub fn wait_ready(max: Duration) {
    let guard = READY.0.lock().unwrap_or_else(|e| e.into_inner());
    let _ = READY.1.wait_timeout_while(guard, max, |ready| !*ready);
}

pub fn info() -> Info {
    INFO.get().cloned().unwrap_or(Info {
        source: "pending".into(),
        ms: 0,
        error: None,
        path: std::env::var("PATH").unwrap_or_default(),
    })
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn with_common(path: &str) -> String {
    merge_common(path, &common_dirs(&user_home()), |p| p.is_dir())
}

/// Step one, on the main thread before anything else runs: the saved PATH
/// (or the current one) plus the common folders. Instant. Then the shell
/// probes run on a background thread, which settles PATH and releases
/// `wait_ready`.
///
/// `set_var` is only called here, before any thread that reads PATH exists,
/// and once more from the probe thread. The second call is acceptable
/// because the engine's background threads block in `wait_ready` until it
/// returns, and nothing else spawned by then reads PATH concurrently: the
/// window isn't open yet (Tauri's builder runs after this) and the UI's
/// commands go through `wait_ready` as well.
pub fn start() {
    if !cfg!(target_os = "macos") {
        let _ = INFO.set(Info {
            source: "system".into(),
            ms: 0,
            error: None,
            path: std::env::var("PATH").unwrap_or_default(),
        });
        mark_ready();
        return;
    }
    let current = std::env::var("PATH").unwrap_or_default();
    if std::env::var_os("DROVR_KEEP_PATH").is_some() {
        let path = with_common(&current);
        std::env::set_var("PATH", &path);
        let _ = INFO.set(Info {
            source: "kept".into(),
            ms: 0,
            error: None,
            path,
        });
        mark_ready();
        return;
    }
    let home = drovr_core::config::home();
    let saved = read_saved(&home);
    let early = with_common(saved.as_deref().unwrap_or(&current));
    std::env::set_var("PATH", &early);
    std::thread::spawn(move || {
        let began = Instant::now();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let r = resolve(
            || probe(&shell, &["-l", "-i"], Duration::from_secs(INTERACTIVE_SECS)),
            || probe(&shell, &["-l"], Duration::from_secs(LOGIN_SECS)),
            saved,
        );
        let ms = began.elapsed().as_millis() as u64;
        let path = with_common(r.path.as_deref().unwrap_or(&current));
        if matches!(r.source, "interactive" | "login") {
            if let Some(p) = &r.path {
                if let Err(e) = write_saved(&home, p) {
                    eprintln!("drovr: couldn't save PATH: {e}");
                }
            }
        }
        std::env::set_var("PATH", &path);
        match &r.error {
            Some(e) => eprintln!("drovr: PATH from {} ({ms} ms): {e}", r.source),
            None => eprintln!("drovr: PATH from {} ({ms} ms)", r.source),
        }
        let _ = INFO.set(Info {
            source: r.source.into(),
            ms,
            error: r.error,
            path,
        });
        mark_ready();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("drovr-lp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_the_marker_amid_shell_noise() {
        let out = "motd\n__DROVR_PATH__/a:/b__DROVR_PATH__\n";
        assert_eq!(parse_marker(out).as_deref(), Some("/a:/b"));
    }

    #[test]
    fn rejects_missing_or_empty_marker() {
        assert_eq!(parse_marker("nothing"), None);
        assert_eq!(parse_marker("__DROVR_PATH____DROVR_PATH__"), None);
    }

    #[test]
    fn merge_appends_dedupes_and_keeps_user_order() {
        let common = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/missing"),
            PathBuf::from("/home/u/.cargo/bin"),
        ];
        let exists = |p: &Path| p != Path::new("/missing");
        let got = merge_common("/usr/local/bin:/x:/usr/bin:/x", &common, exists);
        assert_eq!(
            got,
            "/usr/local/bin:/x:/usr/bin:/opt/homebrew/bin:/home/u/.cargo/bin"
        );
    }

    #[test]
    fn common_dirs_cover_homebrew_and_user_bins() {
        let d = common_dirs(Path::new("/h"));
        let s: Vec<_> = d.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            s,
            [
                "/opt/homebrew/bin",
                "/opt/homebrew/sbin",
                "/usr/local/bin",
                "/h/.local/bin",
                "/h/.opencode/bin",
                "/h/.cargo/bin"
            ]
        );
    }

    #[test]
    fn interactive_wins() {
        let r = resolve(
            || Ok("/i".into()),
            || panic!("not called"),
            Some("/s".into()),
        );
        assert_eq!(
            (r.source, r.path.as_deref(), r.error),
            ("interactive", Some("/i"), None)
        );
    }

    #[test]
    fn login_when_interactive_times_out() {
        let r = resolve(
            || Err("timed out".into()),
            || Ok("/l".into()),
            Some("/s".into()),
        );
        assert_eq!(r.source, "login");
        assert_eq!(r.path.as_deref(), Some("/l"));
        assert!(r.error.unwrap().contains("timed out"));
    }

    #[test]
    fn saved_when_both_fail() {
        let r = resolve(|| Err("a".into()), || Err("b".into()), Some("/s".into()));
        assert_eq!(r.source, "saved");
        assert_eq!(r.path.as_deref(), Some("/s"));
        assert!(r.error.is_some());
    }

    #[test]
    fn system_when_nothing_works() {
        let r = resolve(|| Err("a".into()), || Err("b".into()), None);
        assert_eq!(r.source, "system");
        assert_eq!(r.path, None);
    }

    #[test]
    fn hanging_probes_fall_back_within_the_timeout() {
        let hang = |secs| {
            move || {
                let mut c = Command::new("sh");
                c.arg("-c").arg("sleep 30");
                run_detached(c, Duration::from_millis(secs))
            }
        };
        let began = Instant::now();
        let r = resolve(hang(200), hang(200), Some("/s".into()));
        assert_eq!(r.source, "saved");
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(r.error.unwrap().contains("no answer"));
    }

    #[test]
    fn timed_out_probe_gets_to_clean_up() {
        let d = tmp("term");
        let marker = d.join("cleaned");
        let mut c = Command::new("sh");
        c.arg("-c").arg(format!(
            "trap 'touch {}; exit 0' TERM; sleep 30 & wait",
            marker.display()
        ));
        assert!(run_detached(c, Duration::from_millis(300)).is_err());
        assert!(marker.exists());
    }

    #[test]
    fn probe_has_no_tty_and_no_stdin() {
        let mut c = Command::new("sh");
        c.arg("-c").arg(format!(
            "if (: </dev/tty) 2>/dev/null; then t=tty; else t=none; fi; read x && t=$t-stdin; printf '{MARKER}%s{MARKER}' \"$t-$SSH_ASKPASS_REQUIRE\""
        ));
        assert_eq!(
            run_detached(c, Duration::from_secs(5)).unwrap(),
            "none-never"
        );
    }

    #[test]
    fn saved_path_roundtrips_atomically() {
        let d = tmp("save");
        assert_eq!(read_saved(&d), None);
        write_saved(&d, "/a:/b").unwrap();
        write_saved(&d, "/c:/d").unwrap();
        assert_eq!(read_saved(&d).as_deref(), Some("/c:/d"));
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
        std::fs::write(d.join("login-path"), "\n").unwrap();
        assert_eq!(read_saved(&d), None);
    }
}
