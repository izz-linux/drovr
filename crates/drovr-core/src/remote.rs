//! The phone page: a small web server for checking in on drovr from your
//! phone, and push notifications through ntfy when something needs you.
//!
//! It's a thin layer over the same Engine calls the app's Tauri commands
//! make: the Inbox, agents' output and prompts, reports, code review and the
//! message bus. Every API call needs the access token (a cookie the
//! `/login?token=` link sets, or `Authorization: Bearer`) and an `X-Drovr`
//! header, which a page on another site can't send without a CORS preflight
//! this server never answers.

use crate::config::{self, Remote};
use crate::store::Item;
use crate::{reports, review, Config, Engine};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

mod tls;

const INDEX: &str = include_str!("remote/index.html");
const APP_JS: &str = include_str!("remote/app.js");
const APP_CSS: &str = include_str!("remote/app.css");
const MANIFEST: &str = include_str!("remote/manifest.json");
/// The app's icon, for browser tabs.
const FAVICON: &str = include_str!("remote/favicon.svg");
/// Full-bleed and opaque, cut from `remote/touch-icon.svg`: iOS rounds the
/// corners itself, and draws transparent ones black.
const TOUCH_ICON: &[u8] = include_bytes!("remote/apple-touch-icon.png");
const ICON_192: &[u8] = include_bytes!("remote/icon-192.png");
const ICON_512: &[u8] = include_bytes!("remote/icon-512.png");
const COOKIE: &str = "drovr_remote";
/// The most a request body may be: prompts and answers, not uploads.
const MAX_BODY: u64 = 1 << 20;

type Resp = Response<Cursor<Vec<u8>>>;

// -- the token and the link ----------------------------------------------------

/// The access token: `remote.token`, else the one drovr made and keeps in
/// `remote-token` in its data folder (readable only by you).
pub fn token(cfg: &Config) -> Result<String> {
    let configured = cfg.remote.token.trim();
    if !configured.is_empty() {
        if configured.len() < 16 {
            bail!("remote.token is too short: use at least 16 characters, or leave it empty for drovr to make one");
        }
        return Ok(configured.to_string());
    }
    let path = config::home().join("remote-token");
    if let Some(t) = read_token(&path) {
        return Ok(t);
    }
    let t = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(t.as_bytes())?;
            Ok(t)
        }
        // Another drovr made it first.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_token(&path).ok_or_else(|| anyhow!("{} is empty", path.display()))
        }
        Err(e) => Err(e).with_context(|| format!("writing {}", path.display())),
    }
}

fn read_token(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (t.len() >= 16).then(|| t.to_string())
}

/// Where your phone reaches the page: `remote.public_url`, else this Mac's
/// `.local` name and the port it listens on.
pub fn base_url(r: &Remote) -> String {
    let public = r.public_url.trim().trim_end_matches('/');
    if !public.is_empty() {
        return public.to_string();
    }
    let port = r.bind.rsplit(':').next().unwrap_or("7979");
    let mut host = hostname();
    if !host.contains('.') {
        host.push_str(".local");
    }
    format!("{}://{host}:{port}", r.scheme())
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer outlives the call and its length is passed.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    if ok && !name.is_empty() {
        name
    } else {
        "localhost".into()
    }
}

/// The link that signs your phone in: open it once and the page remembers you.
pub fn link(cfg: &Config) -> Result<String> {
    Ok(format!(
        "{}/login?token={}",
        base_url(&cfg.remote),
        token(cfg)?
    ))
}

/// `text` as a QR code in an SVG, for the app's Settings.
pub fn qr_svg(text: &str) -> String {
    use qrcode::render::svg;
    qrcode::QrCode::new(text.as_bytes())
        .map(|c| {
            c.render::<svg::Color>()
                .min_dimensions(200, 200)
                .dark_color(svg::Color("#000"))
                .light_color(svg::Color("#fff"))
                .build()
        })
        .unwrap_or_default()
}

/// `text` as a QR code drawn with block characters, for a terminal.
pub fn qr_text(text: &str) -> String {
    use qrcode::render::unicode::Dense1x2;
    qrcode::QrCode::new(text.as_bytes())
        .map(|c| {
            c.render::<Dense1x2>()
                .dark_color(Dense1x2::Light)
                .light_color(Dense1x2::Dark)
                .build()
        })
        .unwrap_or_default()
}

// -- status, for the app's Settings --------------------------------------------

static STATUS: Mutex<String> = Mutex::new(String::new());

fn set_status(s: impl Into<String>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
}

/// What the server in this process is doing: `listening on …`, why it
/// couldn't start, or empty when it wasn't started.
pub fn status() -> String {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// -- the server ----------------------------------------------------------------

struct State {
    token: String,
    /// Reports are served under `/plans/<key>/…` so that a report, which runs
    /// sandboxed on no origin and never gets the cookie, can still load its
    /// own images and styles. The key only reads the plans folder, and is new
    /// each time the server starts.
    plans_key: String,
    herdr: Mutex<Option<(std::time::Instant, bool)>>,
    /// Served over HTTPS, so the cookie is only ever sent over it.
    secure: bool,
}

/// Serve the page on `remote.bind`, and send push notifications. Blocks;
/// fails if the address can't be used (another drovr may have it).
pub fn serve(cfg: &Config) -> Result<()> {
    let token = token(cfg)?;
    let bind = cfg.remote.bind.trim().to_string();
    let fail = |msg: String| -> anyhow::Error {
        set_status(&msg);
        anyhow!(msg)
    };
    let tls_files = cfg.remote.tls_files().map_err(|e| fail(format!("{e:#}")))?;
    let secure = tls_files.is_some();
    let server = match tls_files {
        // HTTPS: TLS on `bind`, and the page's server on loopback behind it.
        Some((cert, key)) => {
            let acceptor = tls::Acceptor::new(cert, key).map_err(|e| fail(format!("{e:#}")))?;
            let listener = std::net::TcpListener::bind(&bind)
                .map_err(|e| fail(format!("couldn't listen on {bind}: {e}")))?;
            let server = Server::http("127.0.0.1:0")
                .map_err(|e| fail(format!("couldn't listen on loopback: {e}")))?;
            let backend = server
                .server_addr()
                .to_ip()
                .ok_or_else(|| fail("the page's server has no address".into()))?;
            let acceptor = Arc::new(acceptor);
            std::thread::spawn(move || acceptor.run(listener, backend));
            server
        }
        None => Server::http(&bind).map_err(|e| fail(format!("couldn't listen on {bind}: {e}")))?,
    };
    set_status(format!(
        "listening on {bind} ({})",
        if secure { "HTTPS" } else { "HTTP" }
    ));
    let st = Arc::new(State {
        token,
        plans_key: uuid::Uuid::new_v4().simple().to_string(),
        herdr: Mutex::new(None),
        secure,
    });
    // Only the process that holds the port pushes, so the app and
    // `drovr serve` never both notify you.
    std::thread::spawn(push_loop);
    for req in server.incoming_requests() {
        let st = st.clone();
        std::thread::spawn(move || handle(req, &st));
    }
    Ok(())
}

fn handle(mut req: Request, st: &State) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let q: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let resp = route(&mut req, path, &q, st);
    let _ = req.respond(resp);
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("a valid header")
}

fn reply(status: u16, ctype: &str, body: impl Into<Vec<u8>>) -> Resp {
    Response::from_data(body.into())
        .with_status_code(status)
        .with_header(header("Content-Type", ctype))
        .with_header(header("X-Content-Type-Options", "nosniff"))
        .with_header(header("Referrer-Policy", "no-referrer"))
        .with_header(header("Cache-Control", "no-store"))
}

const PAGE_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; frame-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

fn page(ctype: &str, body: &str) -> Resp {
    reply(200, ctype, body)
        .with_header(header("Content-Security-Policy", PAGE_CSP))
        .with_header(header("X-Frame-Options", "DENY"))
}

/// An icon: public, like the page itself, and fine to cache.
fn icon(png: &'static [u8]) -> Resp {
    Response::from_data(png.to_vec())
        .with_header(header("Content-Type", "image/png"))
        .with_header(header("X-Content-Type-Options", "nosniff"))
        .with_header(header("Cache-Control", "public, max-age=86400"))
}

fn json_reply(status: u16, v: &Value) -> Resp {
    reply(status, "application/json", v.to_string())
}

fn route(req: &mut Request, path: &str, q: &HashMap<String, String>, st: &State) -> Resp {
    let get = *req.method() == Method::Get;
    match path {
        "/" | "/index.html" if get => page("text/html; charset=utf-8", INDEX),
        "/app.js" if get => page("text/javascript; charset=utf-8", APP_JS),
        "/app.css" if get => page("text/css; charset=utf-8", APP_CSS),
        "/manifest.json" if get => page("application/manifest+json", MANIFEST),
        "/favicon.svg" if get => page("image/svg+xml", FAVICON),
        // iOS asks for /apple-touch-icon.png when you add the page to the
        // Home Screen, before (and even without) reading the page's link.
        "/apple-touch-icon.png" | "/apple-touch-icon-precomposed.png" if get => icon(TOUCH_ICON),
        "/icon-192.png" if get => icon(ICON_192),
        "/icon-512.png" if get => icon(ICON_512),
        "/login" if get => login(q.get("token").map(String::as_str).unwrap_or(""), st),
        p if get && p.starts_with("/plans/") => plans_file(&p["/plans/".len()..], st),
        p if p.starts_with("/api/") => {
            if !authorized(req, &st.token) {
                return json_reply(
                    401,
                    &json!({ "error": "sign in with the link from `drovr remote` or drovr's Settings" }),
                );
            }
            if req.headers().iter().all(|h| !h.field.equiv("X-Drovr")) {
                return json_reply(403, &json!({ "error": "missing X-Drovr header" }));
            }
            let body = if get {
                Value::Null
            } else {
                let mut text = String::new();
                if req
                    .as_reader()
                    .take(MAX_BODY)
                    .read_to_string(&mut text)
                    .is_err()
                {
                    return json_reply(400, &json!({ "error": "unreadable body" }));
                }
                serde_json::from_str(&text).unwrap_or(Value::Null)
            };
            match api(&p["/api/".len()..], get, q, body, st) {
                Ok(v) => json_reply(200, &v),
                Err(e) => json_reply(400, &json!({ "error": format!("{e:#}") })),
            }
        }
        _ => reply(404, "text/plain", "not found"),
    }
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

fn authorized(req: &Request, token: &str) -> bool {
    req.headers().iter().any(|h| {
        let v = h.value.as_str();
        if h.field.equiv("Cookie") {
            cookie_value(v, COOKIE).is_some_and(|c| same(c, token))
        } else if h.field.equiv("Authorization") {
            v.strip_prefix("Bearer ")
                .is_some_and(|t| same(t.trim(), token))
        } else {
            false
        }
    })
}

fn login(given: &str, st: &State) -> Resp {
    if !same(given, &st.token) {
        return reply(
            403,
            "text/plain; charset=utf-8",
            "That link's token is wrong or out of date. Get a new one with `drovr remote` or in drovr's Settings.",
        );
    }
    // A year; Lax so tapping a notification arrives signed in.
    let secure = if st.secure { "; Secure" } else { "" };
    let cookie =
        format!("{COOKIE}={given}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Lax{secure}");
    reply(303, "text/plain", "")
        .with_header(header("Set-Cookie", &cookie))
        .with_header(header("Location", "/"))
}

/// A file under the plans folder, sandboxed: scripts in a report run, but on
/// no origin, so they can't reach the API or the cookie.
fn plans_file(rest: &str, st: &State) -> Resp {
    let not_found = || reply(404, "text/plain", "not found");
    let Some((key, rel)) = rest.split_once('/') else {
        return not_found();
    };
    if !same(key, &st.plans_key) {
        return not_found();
    }
    let rel = url::form_urlencoded::parse(format!("p={}", rel.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let Ok(cfg) = Config::load() else {
        return not_found();
    };
    let Some(file) = inside(&cfg.reports.path(), &rel) else {
        return not_found();
    };
    match std::fs::read(&file) {
        Ok(bytes) => reply(200, content_type(&file), bytes)
            .with_header(header(
                "Content-Security-Policy",
                "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox",
            ))
            .with_header(header("X-Frame-Options", "SAMEORIGIN")),
        Err(_) => not_found(),
    }
}

/// `rel` under `dir`, if it's a file there once links are followed.
fn inside(dir: &Path, rel: &str) -> Option<PathBuf> {
    let root = dir.canonicalize().ok()?;
    let p = root.join(rel.trim_start_matches('/')).canonicalize().ok()?;
    (p.starts_with(&root) && p.is_file()).then_some(p)
}

fn content_type(p: &Path) -> &'static str {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "md" | "txt" | "log" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

// -- the API -------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct Args {
    name: String,
    text: String,
    id: i64,
    answer: String,
    reason: String,
    note: String,
    target: String,
    command: String,
    path: String,
    to: String,
    group: String,
    body: String,
}

fn api(
    endpoint: &str,
    get: bool,
    q: &HashMap<String, String>,
    body: Value,
    st: &State,
) -> Result<Value> {
    let e = Engine::open()?;
    let qs = |k: &str| q.get(k).cloned().unwrap_or_default();
    if get {
        return match endpoint {
            "snapshot" => snapshot(&e, st),
            "output" => {
                let lines = qs("lines").parse().unwrap_or(400).min(5000);
                Ok(json!(e.output(&qs("name"), lines)?))
            }
            "reports" => {
                let dir = e.cfg.reports.path();
                let root = dir.canonicalize().unwrap_or(dir.clone());
                let files: Vec<Value> = reports::list(&e.cfg, &e.store)?
                    .into_iter()
                    .map(|f| {
                        let p = Path::new(&f.path);
                        let rel = p
                            .strip_prefix(&root)
                            .or_else(|_| p.strip_prefix(&dir))
                            .map(|r| r.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let mut v = json!(f);
                        v["rel"] = json!(rel);
                        v
                    })
                    .collect();
                Ok(json!({ "key": st.plans_key, "files": files }))
            }
            "review" => {
                let name = qs("name");
                Ok(json!({ "repos": e.review(&name)?, "cwd": e.agent(&name)?.cwd }))
            }
            "diff" => {
                // Only a repo Code review lists for this agent, never any folder.
                let root = qs("root");
                let repo = e
                    .review(&qs("name"))?
                    .into_iter()
                    .find(|r| r.root == root)
                    .ok_or_else(|| anyhow!("{root} isn't one of this agent's repos"))?;
                Ok(json!(review::diff(&repo.root, &qs("path"), &repo.scope)?))
            }
            "bus" => {
                let group = qs("group");
                let g = (!group.is_empty()).then_some(group.as_str());
                Ok(json!({
                    "messages": e.store.messages(g, 300)?,
                    "notes": if group.is_empty() { vec![] } else { e.store.notes(&group)? },
                }))
            }
            other => bail!("unknown endpoint {other}"),
        };
    }
    let a: Args = serde_json::from_value(body).unwrap_or_default();
    Ok(match endpoint {
        "prompt" => json!(e.prompt_from_you(&a.name, &a.text)?),
        "send_now" => json!(e.send_now(&a.name, a.id)?),
        "unqueue" => json!(e.unqueue_by_you(&a.name, a.id)?),
        "stop" => json!(e.stop_by_you(&a.name)?),
        "resume" => json!(e.resume_by_you(&a.name, &a.text)?),
        "answer" => json!(e.answer(a.id, &a.answer)?),
        "reject" => json!(e.reject(a.id, &a.reason)?),
        "ran" => json!(e.mark_ran(a.id, &a.note)?),
        "dismiss" => json!(e.dismiss(a.id)?),
        "run" => {
            let target = if a.target.is_empty() {
                "local"
            } else {
                &a.target
            };
            let command = (!a.command.trim().is_empty()).then_some(a.command.as_str());
            json!(e.run_command(a.id, target, command)?)
        }
        "report_seen" => json!(e.store.mark_report_seen(&a.path)?),
        "message" => json!(e.send_from_you(&a.to, &a.group, &a.body)?),
        other => bail!("unknown endpoint {other}"),
    })
}

fn snapshot(e: &Engine, st: &State) -> Result<Value> {
    let agents: Vec<Value> = e
        .store
        .agents(None)?
        .into_iter()
        .map(|a| {
            let mut v = json!(a);
            v["state"] = json!(e.display_state(&a));
            v["pending"] = json!(e.store.pending_count(&a.name).unwrap_or(0));
            v["queued"] = json!(e.store.queued(&a.name).unwrap_or_default());
            v
        })
        .collect();
    let recent: Vec<Item> = e
        .store
        .items(None, 60)?
        .into_iter()
        .filter(|i| i.status != "pending")
        .rev()
        .take(30)
        .collect();
    Ok(json!({
        "agents": agents,
        "groups": e.store.groups()?,
        "inbox": e.store.items(Some("pending"), 500)?,
        "recent": recent,
        "unseen_reports": reports::unseen(&e.store)?,
        "herdr": herdr_installed(e, st),
        "you": e.cfg.your_name(),
        "cost_warning_usd": e.cfg.defaults.cost_warning_usd,
    }))
}

/// Whether herdr is installed, checked at most every half minute: it starts
/// a shell, and the page asks every few seconds.
fn herdr_installed(e: &Engine, st: &State) -> bool {
    let mut seen = st.herdr.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, found)) = *seen {
        if at.elapsed() < Duration::from_secs(30) {
            return found;
        }
    }
    let found = crate::shell::which(&e.cfg.herdr.command);
    *seen = Some((std::time::Instant::now(), found));
    found
}

// -- push notifications --------------------------------------------------------

/// What's been told already: the newest Inbox item, and the reports.
#[derive(Default)]
struct Pushed {
    started: bool,
    last_item: i64,
    reports: HashSet<String>,
}

fn push_loop() {
    let mut pushed = Pushed::default();
    loop {
        if let Ok(e) = Engine::open() {
            if let Err(err) = push_tick(&e, &mut pushed) {
                eprintln!("drovr: push notification: {err:#}");
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Inbox items newer than `last` that still wait on you, and the newest id seen.
fn fresh(items: &[Item], last: i64) -> (Vec<&Item>, i64) {
    let newest = items.iter().map(|i| i.id).max().unwrap_or(last).max(last);
    let new = items
        .iter()
        .filter(|i| i.id > last && i.status == "pending")
        .collect();
    (new, newest)
}

fn push_tick(e: &Engine, pushed: &mut Pushed) -> Result<()> {
    let items = e.store.items(None, 200)?;
    let unread: Vec<_> = e
        .store
        .report_meta()?
        .into_iter()
        .filter(|m| !m.seen && !m.agent.is_empty())
        .collect();
    // What's already there when drovr starts isn't news.
    if !pushed.started {
        pushed.started = true;
        pushed.last_item = items.iter().map(|i| i.id).max().unwrap_or(0);
        pushed.reports = unread.into_iter().map(|m| m.path).collect();
        return Ok(());
    }
    let (new, newest) = fresh(&items, pushed.last_item);
    pushed.last_item = newest;
    let new_reports: Vec<_> = unread
        .into_iter()
        .filter(|m| pushed.reports.insert(m.path.clone()))
        .collect();
    let r = &e.cfg.remote;
    if r.ntfy_topic.trim().is_empty() || (new.is_empty() && new_reports.is_empty()) {
        return Ok(());
    }
    let base = base_url(r);
    if new.len() > 3 {
        notify(
            r,
            &Note {
                title: "drovr".into(),
                message: format!("{} new items in your Inbox", new.len()),
                click: format!("{base}/#inbox"),
                priority: 4,
                tag: "inbox_tray",
            },
        )?;
    } else {
        for item in new {
            notify(r, &item_note(r, item, &base))?;
        }
    }
    for m in new_reports {
        let name = Path::new(&m.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = if m.title.is_empty() { name } else { m.title };
        let (title, message) = if r.push_details {
            (format!("Report · {}", m.agent), title)
        } else {
            ("drovr".into(), "A new report is ready".into())
        };
        notify(
            r,
            &Note {
                title,
                message,
                click: format!("{base}/#reports"),
                priority: 3,
                tag: "page_facing_up",
            },
        )?;
    }
    Ok(())
}

struct Note {
    title: String,
    message: String,
    click: String,
    priority: u8,
    tag: &'static str,
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "decision" => "Decision",
        "command" => "Command",
        "permission" => "Permission",
        "spawn" => "Spawn",
        "done" => "Finished",
        "watch" => "Watcher",
        "jira" => "Jira draft",
        "handoff" => "Handoff",
        "confluence" => "Confluence",
        other => other,
    }
}

fn item_note(r: &Remote, item: &Item, base: &str) -> Note {
    let urgent = matches!(
        item.kind.as_str(),
        "permission" | "command" | "decision" | "spawn" | "handoff"
    );
    let (title, message) = if r.push_details {
        let text = if item.title.trim().is_empty() {
            item.body.lines().next().unwrap_or("").to_string()
        } else {
            item.title.clone()
        };
        (
            format!("{} · {}", kind_label(&item.kind), item.agent),
            clip(&text, 240),
        )
    } else {
        (
            "drovr".into(),
            "Something new is waiting in your Inbox".into(),
        )
    };
    Note {
        title,
        message,
        click: format!("{base}/#item-{}", item.id),
        priority: if urgent { 4 } else { 3 },
        tag: match item.kind.as_str() {
            "permission" => "lock",
            "command" => "computer",
            "done" => "white_check_mark",
            _ => "inbox_tray",
        },
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Publish to ntfy (its JSON form, so titles needn't be ASCII).
fn notify(r: &Remote, n: &Note) -> Result<()> {
    let body = json!({
        "topic": r.ntfy_topic.trim(),
        "title": n.title,
        "message": n.message,
        "click": n.click,
        "priority": n.priority,
        "tags": [n.tag],
    });
    let mut req =
        ureq::post(r.ntfy_server.trim().trim_end_matches('/')).timeout(Duration::from_secs(10));
    if !r.ntfy_token_env.trim().is_empty() {
        let t = std::env::var(r.ntfy_token_env.trim())
            .map_err(|_| anyhow!("environment variable {} is not set", r.ntfy_token_env))?;
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    req.send_json(body).map_err(|e| anyhow!("ntfy: {e}"))?;
    Ok(())
}

/// Send a test notification, for `drovr remote --test`.
pub fn test_push(cfg: &Config) -> Result<()> {
    if cfg.remote.ntfy_topic.trim().is_empty() {
        bail!("remote.ntfy_topic isn't set");
    }
    let base = base_url(&cfg.remote);
    notify(
        &cfg.remote,
        &Note {
            title: "drovr".into(),
            message: "Push notifications work. Tap to open drovr.".into(),
            click: format!("{base}/"),
            priority: 3,
            tag: "tada",
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn item(id: i64, status: &str) -> Item {
        Item {
            id,
            ts: 0.0,
            agent: "a".into(),
            kind: "permission".into(),
            title: "Bash: ls".into(),
            body: String::new(),
            options: vec![],
            payload: Value::Null,
            status: status.into(),
            answer: String::new(),
            delivered: false,
        }
    }

    #[test]
    fn compares_tokens_exactly() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
        assert!(!same("", "a"));
    }

    #[test]
    fn reads_the_cookie_among_others() {
        let c = "a=1; drovr_remote=tok; b=2";
        assert_eq!(cookie_value(c, COOKIE), Some("tok"));
        assert_eq!(cookie_value("xdrovr_remote=tok", COOKIE), None);
        assert_eq!(cookie_value("", COOKIE), None);
    }

    #[test]
    fn only_new_pending_items_are_news() {
        let items = vec![item(3, "pending"), item(4, "answered"), item(5, "pending")];
        let (new, newest) = fresh(&items, 3);
        assert_eq!(new.iter().map(|i| i.id).collect::<Vec<_>>(), vec![5]);
        assert_eq!(newest, 5);
        let (new, newest) = fresh(&[], 7);
        assert!(new.is_empty());
        assert_eq!(newest, 7);
    }

    #[test]
    fn serves_only_files_inside_the_plans_folder() {
        let dir = std::env::temp_dir().join(format!("drovr-remote-{}", std::process::id()));
        let plans = dir.join("plans");
        std::fs::create_dir_all(plans.join("g")).unwrap();
        std::fs::write(plans.join("g/r.html"), "x").unwrap();
        std::fs::write(dir.join("secret"), "x").unwrap();
        assert!(inside(&plans, "g/r.html").is_some());
        assert!(inside(&plans, "/g/r.html").is_some());
        assert!(inside(&plans, "../secret").is_none());
        assert!(inside(&plans, "g/../../secret").is_none());
        assert!(inside(&plans, "g").is_none());
        std::os::unix::fs::symlink(dir.join("secret"), plans.join("link")).unwrap();
        assert!(inside(&plans, "link").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_link_uses_the_public_url() {
        let mut r = Remote {
            public_url: "http://mac.tail.ts.net:7979/".into(),
            ..Remote::default()
        };
        assert_eq!(base_url(&r), "http://mac.tail.ts.net:7979");
        r.public_url.clear();
        r.bind = "100.64.0.1:8080".into();
        assert!(base_url(&r).ends_with(":8080"));
        assert!(base_url(&r).starts_with("http://"));
        r.tls_cert = "~/c/fullchain.pem".into();
        r.tls_key = "~/c/privkey.pem".into();
        assert!(base_url(&r).starts_with("https://"));
    }

    #[test]
    fn tls_needs_both_files() {
        let mut r = Remote::default();
        assert!(r.tls_files().unwrap().is_none());
        r.tls_cert = "/c/fullchain.pem".into();
        assert!(r.tls_files().is_err());
        r.tls_key = "~/c/privkey.pem".into();
        let (cert, key) = r.tls_files().unwrap().unwrap();
        assert_eq!(cert, PathBuf::from("/c/fullchain.pem"));
        assert!(!key.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn a_missing_certificate_says_which_file() {
        let e = tls::load(
            Path::new("/nope/fullchain.pem"),
            Path::new("/nope/privkey.pem"),
        )
        .err()
        .unwrap();
        assert!(format!("{e:#}").contains("/nope/fullchain.pem"));
    }

    #[test]
    fn notifications_can_leave_out_details() {
        let mut r = Remote::default();
        let i = item(9, "pending");
        let n = item_note(&r, &i, "http://m:1");
        assert_eq!(n.title, "Permission · a");
        assert_eq!(n.message, "Bash: ls");
        assert_eq!(n.click, "http://m:1/#item-9");
        assert_eq!(n.priority, 4);
        r.push_details = false;
        let n = item_note(&r, &i, "http://m:1");
        assert!(!n.message.contains("Bash"));
        assert_eq!(n.title, "drovr");
    }

    #[test]
    fn icons_are_the_sizes_they_claim() {
        // A PNG's width and height are big-endian at bytes 16..24.
        let size = |png: &[u8]| {
            assert_eq!(&png[1..4], b"PNG");
            let n = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            (n(&png[16..20]), n(&png[20..24]))
        };
        assert_eq!(size(TOUCH_ICON), (180, 180));
        assert_eq!(size(ICON_192), (192, 192));
        assert_eq!(size(ICON_512), (512, 512));
        assert!(INDEX.contains(r#"rel="apple-touch-icon" href="/apple-touch-icon.png""#));
    }

    #[test]
    fn clips_long_text() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
