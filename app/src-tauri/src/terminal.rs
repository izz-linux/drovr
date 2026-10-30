//! Terminals in the app: a login shell in a pseudo-terminal, streamed to
//! xterm.js in the window. A group shares one terminal; a solo agent has its
//! own. Shells keep running while the panel is hidden and end with the app.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// Output kept per terminal, for reading back what a command printed.
const KEEP: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub struct Terminals(Mutex<HashMap<String, Term>>);

struct Term {
    id: u64,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    out: Arc<Mutex<Output>>,
}

#[derive(Default)]
struct Output {
    /// The last `KEEP` bytes of output.
    buf: Vec<u8>,
    /// Bytes written since the shell started.
    total: u64,
}

#[derive(Clone, Serialize)]
struct Data {
    key: String,
    data: String,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(2),
        cols: cols.max(10),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Open the terminal `key`, or return `false` if it's already running.
#[tauri::command]
pub fn term_open(
    app: AppHandle,
    terms: State<Terminals>,
    key: String,
    cwd: String,
    cols: u16,
    rows: u16,
) -> Result<bool, String> {
    let mut map = terms.0.lock().unwrap();
    if map.contains_key(&key) {
        return Ok(false);
    }
    let cfg = drovr_core::Config::load().unwrap_or_default();
    let pair = native_pty_system()
        .openpty(size(cols, rows))
        .map_err(|e| e.to_string())?;
    // A login shell, as iTerm2 starts one: it reads .zprofile and .zshrc, so
    // your aliases, functions, prompt and PATH are all there.
    let mut cmd = CommandBuilder::new(cfg.terminal.shell());
    cmd.arg("-l");
    let home = drovr_core::config::user_home();
    let dir = drovr_core::config::expand_tilde(&cwd);
    cmd.cwd(if !dir.is_empty() && Path::new(&dir).is_dir() {
        Path::new(&dir).to_path_buf()
    } else {
        home
    });
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "drovr");
    // Apps opened from Finder get no locale; without one zsh mangles UTF-8.
    if std::env::var("LANG").map(|l| l.is_empty()).unwrap_or(true) {
        cmd.env("LANG", "en_US.UTF-8");
    }
    let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let out = Arc::new(Mutex::new(Output::default()));
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    map.insert(
        key.clone(),
        Term {
            id,
            master: pair.master,
            writer,
            child,
            out: out.clone(),
        },
    );

    std::thread::spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        // Bytes of a UTF-8 character split across reads.
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            {
                let mut o = out.lock().unwrap();
                o.buf.extend_from_slice(&chunk[..n]);
                o.total += n as u64;
                if o.buf.len() > KEEP {
                    let cut = o.buf.len() - KEEP;
                    o.buf.drain(..cut);
                }
            }
            carry.extend_from_slice(&chunk[..n]);
            let valid = match std::str::from_utf8(&carry) {
                Ok(_) => carry.len(),
                Err(e) if e.error_len().is_none() => e.valid_up_to(),
                Err(_) => carry.len(),
            };
            let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
            carry.drain(..valid);
            if !text.is_empty() {
                let _ = app.emit(
                    "term-data",
                    Data {
                        key: key.clone(),
                        data: text,
                    },
                );
            }
        }
        // The shell exited: forget it and say so, unless it was closed on
        // purpose (term_close, a restart) or the key was reopened since.
        let terms = app.state::<Terminals>();
        let mut map = terms.0.lock().unwrap();
        let current = map.get(&key).map(|t| t.id) == Some(id);
        if current {
            if let Some(mut t) = map.remove(&key) {
                let _ = t.child.wait();
            }
        }
        drop(map);
        if current {
            let _ = app.emit("term-exit", key);
        }
    });
    Ok(true)
}

#[tauri::command]
pub fn term_write(terms: State<Terminals>, key: String, data: String) -> Result<(), String> {
    let mut map = terms.0.lock().unwrap();
    let t = map.get_mut(&key).ok_or("terminal not running")?;
    t.writer
        .write_all(data.as_bytes())
        .and_then(|_| t.writer.flush())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn term_resize(terms: State<Terminals>, key: String, cols: u16, rows: u16) {
    if let Some(t) = terms.0.lock().unwrap().get(&key) {
        let _ = t.master.resize(size(cols, rows));
    }
}

/// A position in the terminal's output, to read what was printed after it.
#[tauri::command]
pub fn term_mark(terms: State<Terminals>, key: String) -> u64 {
    terms
        .0
        .lock()
        .unwrap()
        .get(&key)
        .map(|t| t.out.lock().unwrap().total)
        .unwrap_or(0)
}

/// What the terminal printed since `mark`, as plain text (at most `max` chars).
#[tauri::command]
pub fn term_since(terms: State<Terminals>, key: String, mark: u64, max: usize) -> String {
    let map = terms.0.lock().unwrap();
    let Some(t) = map.get(&key) else {
        return String::new();
    };
    let o = t.out.lock().unwrap();
    let start = o.total - o.buf.len() as u64;
    let from = mark.saturating_sub(start).min(o.buf.len() as u64) as usize;
    let text = plain(&String::from_utf8_lossy(&o.buf[from..]));
    let chars: Vec<char> = text.trim().chars().collect();
    chars[chars.len().saturating_sub(max)..].iter().collect()
}

/// The job running in the foreground, when it isn't the shell itself: a
/// command, or a whole pipeline, which share one process group.
fn foreground_job(t: &Term) -> Option<i32> {
    let group = t.master.process_group_leader()?;
    let shell = t.child.process_id().map(|p| p as i32);
    (Some(group) != shell).then_some(group)
}

/// Force-stop the foreground job, for one that ignores ^C. The shell and
/// its scrollback stay. Returns whether something was running.
#[tauri::command]
pub fn term_stop_job(terms: State<Terminals>, key: String) -> bool {
    let map = terms.0.lock().unwrap();
    match map.get(&key).and_then(foreground_job) {
        Some(group) => {
            unsafe { libc::killpg(group, libc::SIGKILL) };
            true
        }
        None => false,
    }
}

/// End the shell and anything running in it.
#[tauri::command]
pub fn term_close(terms: State<Terminals>, key: String) {
    if let Some(mut t) = terms.0.lock().unwrap().remove(&key) {
        if let Some(group) = foreground_job(&t) {
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
        let _ = t.child.kill();
    }
}

/// End every shell when the app quits.
pub fn close_all(app: &AppHandle) {
    let terms = app.state::<Terminals>();
    for (_, mut t) in terms.0.lock().unwrap().drain() {
        let _ = t.child.kill();
    }
}

/// Terminal output without escape sequences or carriage returns.
pub fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: ESC [ params… final byte
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC (titles, shell integration): ends with BEL or ESC \
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Character set selection takes one more character.
                Some('(') | Some(')') => {
                    chars.next();
                }
                _ => {}
            },
            '\r' => {}
            '\x07' | '\x08' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn strips_escapes() {
        let s = "\x1b]0;~/src\x07\x1b[1;32m➜\x1b[0m  ls\r\nREADME.md\r\n\x1b(B\x1b[m% ";
        assert_eq!(plain(s), "➜  ls\nREADME.md\n% ");
    }
}
