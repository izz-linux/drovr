//! Tauri commands: a thin layer over drovr-core.
//!
//! Each command opens its own Engine (a SQLite handle plus the config), so
//! config edits apply immediately and nothing holds a lock across slow calls
//! such as an agent querying Jira through its MCP.

mod menu;
mod terminal;

use drovr_core::engine::{slug, Line, NewGroup};
use drovr_core::reports::ReportFile;
use drovr_core::review::{self, Repo};
use drovr_core::shell::quote;
use drovr_core::sources::{ticket_task, Page, SlackMessage, Sources, Ticket};
use drovr_core::store::{Agent, Group, Item, Message, Note};
use drovr_core::{config, Engine, NewAgent};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

type Res<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce(&Engine) -> anyhow::Result<T> + Send + 'static,
) -> Res<T> {
    tauri::async_runtime::spawn_blocking(move || {
        let engine = Engine::open()?;
        f(&engine)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
struct AgentView {
    #[serde(flatten)]
    agent: Agent,
    state: String,
    pending: i64,
    /// Your messages it hasn't read yet.
    queued: Vec<Message>,
}

#[derive(Serialize)]
struct Snapshot {
    agents: Vec<AgentView>,
    groups: Vec<Group>,
    inbox: Vec<Item>,
    recent: Vec<Item>,
    unseen_reports: i64,
    herdr: bool,
    /// Whether the Codex CLI is installed, for agents with harness "codex".
    codex: bool,
}

#[tauri::command]
async fn snapshot() -> Res<Snapshot> {
    blocking(|e| {
        let agents = e
            .store
            .agents(None)?
            .into_iter()
            .map(|a| AgentView {
                state: e.display_state(&a),
                pending: e.store.pending_count(&a.name).unwrap_or(0),
                queued: e.store.queued(&a.name).unwrap_or_default(),
                agent: a,
            })
            .collect();
        let recent = e
            .store
            .items(None, 60)?
            .into_iter()
            .filter(|i| i.status != "pending")
            .rev()
            .take(30)
            .collect();
        Ok(Snapshot {
            agents,
            groups: e.store.groups()?,
            inbox: e.store.items(Some("pending"), 500)?,
            recent,
            unseen_reports: e.store.unseen_reports()?,
            herdr: installed(&e.cfg.herdr.command),
            codex: installed(&e.cfg.codex.command),
        })
    })
    .await
}

/// Whether a command (herdr, codex) is installed. Checking starts a shell
/// and the window asks every two seconds, so answers are kept for half a
/// minute.
fn installed(command: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static SEEN: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some((at, found)) = seen.lock().unwrap_or_else(|e| e.into_inner()).get(command) {
        if at.elapsed() < Duration::from_secs(30) {
            return *found;
        }
    }
    let found = drovr_core::shell::which(command);
    seen.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(command.to_string(), (Instant::now(), found));
    found
}

// -- agents -------------------------------------------------------------------

#[tauri::command]
async fn agent_output(name: String, lines: usize) -> Res<Vec<Line>> {
    blocking(move |e| e.output(&name, lines)).await
}

#[tauri::command]
async fn new_agent(spec: NewAgent) -> Res<Agent> {
    blocking(move |e| e.start_agent(spec)).await
}

/// Create a group. Returns its orchestrator when one was requested.
#[tauri::command]
async fn create_group(spec: NewGroup) -> Res<Option<Agent>> {
    blocking(move |e| e.create_group(spec)).await
}

#[tauri::command]
async fn add_lead(group: String, goal: String, backend: String, harness: String) -> Res<Agent> {
    blocking(move |e| e.add_lead(&group, &goal, &backend, &harness)).await
}

/// The values a new agent gets when a field is left alone.
#[derive(Serialize)]
struct Defaults {
    cwd: String,
    backend: String,
    harness: String,
    model: String,
    permission_mode: String,
    worktrees: bool,
    plans_dir: String,
    your_name: String,
    /// Notify when something new lands in the Inbox while drovr is in the background.
    notify: bool,
    /// An agent whose runs cost more than this is flagged; 0 turns it off.
    cost_warning_usd: f64,
    /// Copy text you select in an agent's output.
    copy_on_select: bool,
    /// The ticket workflow (record, timeline) is on.
    workflow: bool,
    /// The skill agents follow to write a ticket for their task (jira.ticket_skill).
    ticket_skill: String,
}

#[tauri::command]
async fn defaults() -> Res<Defaults> {
    blocking(|e| {
        let d = &e.cfg.defaults;
        Ok(Defaults {
            cwd: e.cfg.default_cwd(),
            backend: d.backend.clone(),
            harness: d.harness.clone(),
            model: d.model.clone(),
            permission_mode: d.permission_mode.clone(),
            worktrees: d.worktrees,
            plans_dir: e.cfg.reports.path().display().to_string(),
            your_name: e.cfg.your_name(),
            notify: e.cfg.notifications.enabled,
            cost_warning_usd: d.cost_warning_usd,
            copy_on_select: d.copy_on_select,
            workflow: e.cfg.workflow.enabled,
            ticket_skill: e.cfg.jira.ticket_skill.clone(),
        })
    })
    .await
}

/// Web links open in your browser, never inside drovr: links in agent
/// output, the Inbox, report previews (their iframes navigate through this
/// too) and links that try to open a new window.
fn links_in_browser<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("links-in-browser")
        .on_navigation(|webview, url| {
            use tauri::Manager;
            if !is_web_link(url, webview.config().build.dev_url.as_ref()) {
                return true;
            }
            use tauri_plugin_opener::OpenerExt;
            let _ = webview.opener().open_url(url.as_str(), None::<&str>);
            false
        })
        .build()
}

/// The window from tauri.conf.json (which has `create: false`), built here
/// so links that ask for a new window (`target="_blank"`, `window.open`)
/// open in your browser instead.
fn open_main_window(app: &tauri::App) -> tauri::Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
        .unwrap_or_default();
    let handle = app.handle().clone();
    tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
        .on_new_window(move |url, _| {
            if is_web_link(&url, handle.config().build.dev_url.as_ref()) {
                let _ = handle.opener().open_url(url.as_str(), None::<&str>);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .build()?;
    Ok(())
}

/// Whether a navigation leaves drovr: http(s) anywhere but drovr's own pages
/// (the dev server, and tauri.localhost / asset.localhost where the app and
/// report previews are served on some platforms), or mailto:/tel:.
fn is_web_link(url: &tauri::Url, dev_url: Option<&tauri::Url>) -> bool {
    match url.scheme() {
        "mailto" | "tel" => true,
        "http" | "https" => {
            let own = matches!(
                url.host_str(),
                Some("tauri.localhost" | "asset.localhost" | "ipc.localhost")
            );
            let dev = dev_url.is_some_and(|d| d.origin() == url.origin());
            !own && !dev
        }
        _ => false,
    }
}

/// Until 1.0.0 the app's identifier was `com.drovr.app`, so its saved
/// settings (the theme) were in that folder. Move them to the new one once.
fn move_old_settings(app: &tauri::App) {
    use tauri::Manager;
    let Ok(new) = app.path().app_data_dir() else {
        return;
    };
    let Some(old) = new.parent().map(|p| p.join("com.drovr.app")) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&old) else {
        return;
    };
    // The webview may already have made the new folder; move what isn't there.
    let _ = std::fs::create_dir_all(&new);
    for e in entries.flatten() {
        let to = new.join(e.file_name());
        if !to.exists() {
            let _ = std::fs::rename(e.path(), to);
        }
    }
    let _ = std::fs::remove_dir(&old);
}

/// Where an agent's terminal opens: the repo it worked in most recently.
#[tauri::command]
async fn term_workspace(name: String) -> Res<String> {
    blocking(move |e| e.workspace(&name)).await
}

#[derive(Serialize)]
struct TermSettings {
    font_family: String,
    font_size: u32,
    option_as_meta: bool,
    copy_on_select: bool,
    /// The shell's program name (`zsh`), so the panel knows how it signals
    /// that it's ready for a command.
    shell: String,
}

#[tauri::command]
async fn term_settings() -> Res<TermSettings> {
    blocking(|e| {
        let t = &e.cfg.terminal;
        Ok(TermSettings {
            font_family: t.font_family.clone(),
            font_size: t.font_size,
            option_as_meta: t.option_as_meta,
            copy_on_select: t.copy_on_select,
            shell: std::path::Path::new(&t.shell())
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    })
    .await
}

/// Recent Claude Code sessions, to start an agent from one.
#[tauri::command]
async fn claude_sessions() -> Res<Vec<drovr_core::sessions::SavedSession>> {
    use drovr_core::harness::Harness;
    blocking(|_| Ok(drovr_core::harness::CLAUDE.recent_sessions(40))).await
}

#[tauri::command]
async fn find_session(id: String) -> Res<Option<drovr_core::sessions::SavedSession>> {
    use drovr_core::harness::Harness;
    blocking(move |_| Ok(drovr_core::harness::CLAUDE.find_session(id.trim()).ok())).await
}

#[tauri::command]
async fn plans_folders() -> Res<Vec<drovr_core::reports::PlansFolder>> {
    blocking(|e| Ok(drovr_core::reports::folders(&e.cfg))).await
}

#[tauri::command]
async fn read_prompt(folder: String, file: String) -> Res<String> {
    blocking(move |e| drovr_core::reports::read_prompt(&e.cfg, &folder, &file)).await
}

#[tauri::command]
async fn prompt_agent(name: String, text: String) -> Res<String> {
    blocking(move |e| e.prompt_from_you(&name, &text)).await
}

#[tauri::command]
async fn send_now(name: String, id: i64) -> Res<String> {
    blocking(move |e| e.send_now(&name, id)).await
}

#[tauri::command]
async fn unqueue(name: String, id: i64) -> Res<bool> {
    blocking(move |e| e.unqueue_by_you(&name, id)).await
}

#[tauri::command]
async fn stop_agent(name: String) -> Res<()> {
    blocking(move |e| e.stop_by_you(&name)).await
}

#[tauri::command]
async fn stop_group(group: String) -> Res<()> {
    blocking(move |e| {
        for a in e.store.agents(Some(&group))? {
            e.stop_by_you(&a.name)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn resume_agent(name: String, text: String) -> Res<String> {
    blocking(move |e| e.resume_by_you(&name, &text)).await
}

/// Returns a note about anything left behind, such as a worktree with
/// uncommitted changes.
#[tauri::command]
async fn remove_agent(name: String) -> Res<String> {
    blocking(move |e| e.remove(&name)).await
}

#[tauri::command]
async fn remove_group(group: String) -> Res<String> {
    blocking(move |e| {
        let mut notes = vec![];
        for a in e.store.agents(Some(&group))? {
            let note = e.remove(&a.name)?;
            if !note.is_empty() {
                notes.push(format!("{}: {note}", a.name));
            }
        }
        e.store.remove_group(&group)?;
        Ok(notes.join("\n"))
    })
    .await
}

#[tauri::command]
async fn attach_command(name: String) -> Res<String> {
    blocking(move |e| e.attach_command(&name)).await
}

#[tauri::command]
async fn open_in_herdr(name: String) -> Res<String> {
    blocking(move |e| e.open_in_herdr(&name)).await
}

// -- inbox --------------------------------------------------------------------

#[tauri::command]
async fn inbox_answer(id: i64, answer: String) -> Res<()> {
    blocking(move |e| e.answer(id, &answer)).await
}

#[tauri::command]
async fn inbox_reject(id: i64, reason: String) -> Res<()> {
    blocking(move |e| e.reject(id, &reason)).await
}

#[tauri::command]
async fn inbox_mark_ran(id: i64, note: String) -> Res<()> {
    blocking(move |e| e.mark_ran(id, &note)).await
}

#[tauri::command]
async fn inbox_dismiss(id: i64) -> Res<()> {
    blocking(move |e| e.dismiss(id)).await
}

#[tauri::command]
async fn run_command(id: i64, target: String, command: String) -> Res<String> {
    blocking(move |e| e.run_command(id, &target, Some(&command))).await
}

// -- bus ----------------------------------------------------------------------

#[derive(Serialize)]
struct Bus {
    messages: Vec<Message>,
    notes: Vec<Note>,
}

#[tauri::command]
async fn bus(group: String) -> Res<Bus> {
    blocking(move |e| {
        let g = if group.is_empty() {
            None
        } else {
            Some(group.as_str())
        };
        Ok(Bus {
            messages: e.store.messages(g, 300)?,
            notes: if group.is_empty() {
                vec![]
            } else {
                e.store.notes(&group)?
            },
        })
    })
    .await
}

/// Post as the human: to an agent, `@group` (with `group`), or `*`.
#[tauri::command]
async fn send_message(to: String, group: String, body: String) -> Res<()> {
    blocking(move |e| e.send_from_you(&to, &group, &body)).await
}

// -- sources ------------------------------------------------------------------

/// Run a lookup, emitting `lookup-progress` events as it goes.
async fn lookup<T: Send + 'static>(
    app: tauri::AppHandle,
    f: impl FnOnce(&Sources) -> anyhow::Result<T> + Send + 'static,
) -> Res<T> {
    use tauri::Emitter;
    blocking(move |e| {
        let progress = move |text: &str| {
            let _ = app.emit("lookup-progress", text.to_string());
        };
        f(&Sources::with_progress(&e.cfg, &progress))
    })
    .await
}

#[tauri::command]
async fn cancel_lookups() -> Res<usize> {
    Ok(drovr_core::sources::cancel_lookups())
}

#[tauri::command]
async fn jira_search(app: tauri::AppHandle, query: String) -> Res<Vec<Ticket>> {
    lookup(app, move |src| src.jira_search(&query)).await
}

#[tauri::command]
async fn jira_get(app: tauri::AppHandle, key: String) -> Res<Ticket> {
    lookup(app, move |src| src.jira_get(&key)).await
}

/// Hand a ticket to a new solo agent or a new group with an orchestrator.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn dispatch_ticket(
    ticket: Ticket,
    mode: String,
    name: String,
    cwd: String,
    backend: String,
    harness: String,
    notes: String,
    group: String,
    target: String,
) -> Res<Agent> {
    blocking(move |e| {
        let name = if name.trim().is_empty() {
            slug(&ticket.key)
        } else {
            name.trim().to_string()
        };
        let task = ticket_task(&ticket, &notes, &e.cfg.your_name());
        let agent = if mode == "existing" {
            e.hand_off_to_group(&group, &ticket.key, &task, &target)
        } else if mode == "group" {
            let spec = NewGroup {
                name,
                goal: task,
                ticket: ticket.key.clone(),
                cwd,
                backend,
                harness,
                lead: true,
                ..NewGroup::default()
            };
            e.create_group(spec)?
                .ok_or_else(|| anyhow::anyhow!("the orchestrator didn't start"))
        } else {
            e.start_agent(NewAgent {
                name,
                cwd,
                backend,
                harness,
                ticket: ticket.key.clone(),
                task,
                ..NewAgent::default()
            })
        }?;
        // With watch.dispatch_transition set, the ticket moves (to In Progress).
        drovr_core::rules::move_on_dispatch(e, &ticket.key);
        Ok(agent)
    })
    .await
}

#[tauri::command]
async fn confluence_search(app: tauri::AppHandle, query: String) -> Res<Vec<Page>> {
    lookup(app, move |src| src.confluence_search(&query)).await
}

#[tauri::command]
async fn confluence_get(app: tauri::AppHandle, id: String) -> Res<Page> {
    lookup(app, move |src| src.confluence_get(&id)).await
}

#[tauri::command]
async fn slack_search(app: tauri::AppHandle, query: String) -> Res<Vec<SlackMessage>> {
    lookup(app, move |src| src.slack_search(&query)).await
}

/// Free-form question to a one-shot agent with your MCPs.
#[tauri::command]
async fn ask(app: tauri::AppHandle, question: String) -> Res<String> {
    lookup(app, move |src| src.ask(&question, None)).await
}

// -- review -------------------------------------------------------------------

#[derive(Serialize)]
struct ReviewInfo {
    cwd: String,
    repos: Vec<Repo>,
}

#[tauri::command]
async fn review(app: tauri::AppHandle, name: String) -> Res<ReviewInfo> {
    let info = blocking(move |e| {
        Ok(ReviewInfo {
            repos: e.review(&name)?,
            cwd: e.agent(&name)?.cwd,
        })
    })
    .await?;
    for r in &info.repos {
        allow_previews(&app, Path::new(&r.root));
    }
    Ok(info)
}

#[tauri::command]
async fn review_diff(root: String, path: String, scope: Vec<String>) -> Res<String> {
    blocking(move |_| review::diff(&root, &path, &scope)).await
}

// -- the record ---------------------------------------------------------------

/// Every ticket with a record, most recently updated first.
#[tauri::command]
async fn records(app: tauri::AppHandle) -> Res<Vec<drovr_core::ledger::Record>> {
    let (dir, list) =
        blocking(|e| Ok((e.cfg.reports.path(), drovr_core::ledger::list(&e.cfg)))).await?;
    // So Show folder can open a ticket's folder.
    allow_previews(&app, &dir);
    Ok(list)
}

/// A ticket's record, oldest event first.
#[tauri::command]
async fn timeline(ticket: String) -> Res<Vec<Value>> {
    blocking(move |e| Ok(drovr_core::ledger::timeline(&e.cfg, &ticket))).await
}

/// What the watcher last saw: your sprint's tickets and their PRs.
#[tauri::command]
async fn watch_state() -> Res<drovr_core::watch::State> {
    Ok(drovr_core::watch::load())
}

/// Read Jira and GitHub now.
#[tauri::command]
async fn watch_now() -> Res<drovr_core::watch::State> {
    blocking(|e| {
        if !e.cfg.workflow.enabled {
            anyhow::bail!("the ticket workflow is off (workflow.enabled)");
        }
        Ok(drovr_core::watch::poll(e, true))
    })
    .await
}

/// Start a retro on these tickets.
#[tauri::command]
async fn start_retro(tickets: Vec<String>, note: String) -> Res<Agent> {
    blocking(move |e| e.start_retro(&tickets, &note)).await
}

/// Every retro, newest first, with its proposals.
#[tauri::command]
async fn retros() -> Res<Vec<drovr_core::retro::Retro>> {
    blocking(|e| Ok(e.retros())).await
}

/// Approve (and apply) or dismiss one of a retro's proposals.
#[tauri::command]
async fn decide_proposal(dir: String, id: String, approve: bool) -> Res<String> {
    blocking(move |e| e.decide_proposal(&dir, &id, approve)).await
}

// -- reports ------------------------------------------------------------------

#[derive(Serialize)]
struct Reports {
    dir: String,
    files: Vec<ReportFile>,
}

#[tauri::command]
async fn reports(app: tauri::AppHandle) -> Res<Reports> {
    let reports = blocking(|e| {
        Ok(Reports {
            dir: e.cfg.reports.path().display().to_string(),
            files: drovr_core::reports::list(&e.cfg, &e.store)?,
        })
    })
    .await?;
    allow_previews(&app, Path::new(&reports.dir));
    Ok(reports)
}

#[tauri::command]
async fn report_seen(path: String) -> Res<()> {
    blocking(move |e| e.store.mark_report_seen(&path)).await
}

#[tauri::command]
async fn reports_all_seen() -> Res<()> {
    blocking(|e| e.store.mark_all_reports_seen()).await
}

/// Let previews load the files under `dir` through the asset protocol. Its
/// scope starts empty, so it only ever serves the plans folder and the repos
/// you open in Code review, never the rest of your disk.
fn allow_previews(app: &tauri::AppHandle, dir: &Path) {
    use tauri::Manager;
    // The protocol checks the canonical path of what's asked for.
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return;
    };
    let scope = app.asset_protocol_scope();
    if !scope.is_allowed(&dir) {
        let _ = scope.allow_directory(&dir, true);
    }
}

/// Open a folder in Finder or an HTML page in your browser, from the plans
/// folder or a reviewed repo. Nothing that would run: no app bundles,
/// scripts or `.command` files, wherever they are.
#[tauri::command]
async fn open_path(app: tauri::AppHandle, path: String) -> Res<()> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;
    let p = std::fs::canonicalize(&path).map_err(|e| format!("{path}: {e}"))?;
    if !openable(&p) || !app.asset_protocol_scope().is_allowed(&p) {
        return Err(format!(
            "drovr only opens folders and HTML pages in the plans folder and reviewed repos, not {path}"
        ));
    }
    app.opener()
        .open_path(p.display().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// A plain folder, or an HTML page.
fn openable(p: &Path) -> bool {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    if p.is_dir() {
        // macOS opens a bundle (Foo.app, Foo.workflow…) by running it.
        const BUNDLES: &[&str] = &[
            "app",
            "bundle",
            "framework",
            "plugin",
            "kext",
            "prefpane",
            "workflow",
            "action",
            "saver",
            "appex",
            "xpc",
            "qlgenerator",
            "mdimporter",
            "service",
            "pkg",
            "mpkg",
        ];
        !BUNDLES.contains(&ext.as_str()) && !p.join("Contents/Info.plist").exists()
    } else {
        p.is_file() && matches!(ext.as_str(), "html" | "htm")
    }
}

// -- settings -----------------------------------------------------------------

#[tauri::command]
async fn config_info() -> Res<Value> {
    blocking(|e| {
        let path = config::config_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let effective = toml_string(&e.cfg);
        Ok(json!({ "path": path, "home": config::home(), "text": text, "effective": effective }))
    })
    .await
}

fn toml_string(cfg: &drovr_core::Config) -> String {
    serde_json::to_value(cfg)
        .ok()
        .and_then(|v| drovr_core::config::to_toml(&v).ok())
        .unwrap_or_default()
}

#[tauri::command]
async fn save_config(text: String) -> Res<()> {
    blocking(move |_| {
        drovr_core::config::parse(&text)?;
        let path = config::config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    })
    .await
}

/// Apps launched from Finder or the Dock get a minimal PATH
/// (/usr/bin:/bin:/usr/sbin:/sbin), so `claude`, `herdr`, `node` and
/// Homebrew tools wouldn't be found. Take PATH from the user's login shell.
fn adopt_login_shell_path() {
    if !cfg!(target_os = "macos") || std::env::var_os("DROVR_KEEP_PATH").is_some() {
        return;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let script = "printf '__DROVR_PATH__%s__DROVR_PATH__' \"$PATH\"";
    // Interactive so ~/.zshrc runs too; bounded in case it waits on something.
    let cmd = format!("{} -l -i -c {}", quote(&shell), quote(script));
    let Ok(out) = drovr_core::shell::sh(&cmd, None, None, Duration::from_secs(5)) else {
        return;
    };
    if let Some(path) = out
        .stdout
        .split("__DROVR_PATH__")
        .nth(1)
        .filter(|p| !p.is_empty())
    {
        std::env::set_var("PATH", path);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    adopt_login_shell_path();
    // Notice headless runs that died without reporting back, and track herdr panes.
    std::thread::spawn(|| loop {
        if let Ok(e) = Engine::open() {
            let _ = e.refresh();
            let _ = e.refresh_herdr();
        }
        std::thread::sleep(Duration::from_secs(4));
    });
    // The ticket workflow's watcher: Jira and PR status, polled as each
    // falls due (watch.jira_poll_secs, watch.pr_poll_secs).
    std::thread::spawn(|| loop {
        if let Ok(e) = Engine::open() {
            if e.cfg.workflow.enabled {
                drovr_core::watch::poll(&e, false);
            }
        }
        std::thread::sleep(Duration::from_secs(15));
    });
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(links_in_browser())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(terminal::Terminals::default())
        .setup(|app| {
            move_old_settings(app);
            open_main_window(app)?;
            menu::build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            menu::load_theme,
            menu::save_theme,
            snapshot,
            agent_output,
            new_agent,
            create_group,
            add_lead,
            defaults,
            plans_folders,
            claude_sessions,
            find_session,
            read_prompt,
            prompt_agent,
            send_now,
            unqueue,
            records,
            timeline,
            watch_state,
            watch_now,
            start_retro,
            retros,
            decide_proposal,
            stop_agent,
            stop_group,
            resume_agent,
            remove_agent,
            remove_group,
            attach_command,
            open_in_herdr,
            inbox_answer,
            inbox_reject,
            inbox_mark_ran,
            inbox_dismiss,
            run_command,
            bus,
            send_message,
            jira_search,
            cancel_lookups,
            jira_get,
            dispatch_ticket,
            confluence_search,
            confluence_get,
            slack_search,
            ask,
            review,
            review_diff,
            config_info,
            save_config,
            reports,
            report_seen,
            reports_all_seen,
            open_path,
            term_workspace,
            term_settings,
            terminal::term_open,
            terminal::term_write,
            terminal::term_resize,
            terminal::term_mark,
            terminal::term_since,
            terminal::term_stop_job,
            terminal::term_close
        ])
        .build(tauri::generate_context!())
        .expect("error while building drovr")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                terminal::close_all(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::is_web_link;
    use tauri::Url;

    #[test]
    fn opens_only_folders_and_pages() {
        use super::openable;
        let dir = std::env::temp_dir().join(format!("drovr-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plans v1.2")).unwrap();
        std::fs::create_dir_all(dir.join("Evil.app/Contents")).unwrap();
        std::fs::create_dir_all(dir.join("Sneaky/Contents")).unwrap();
        std::fs::write(dir.join("Sneaky/Contents/Info.plist"), "").unwrap();
        for f in ["report.html", "page.HTM", "run.command", "x.sh"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert!(openable(&dir.join("plans v1.2")));
        assert!(openable(&dir.join("report.html")));
        assert!(openable(&dir.join("page.HTM")));
        for bad in ["Evil.app", "Sneaky", "run.command", "x.sh", "missing.html"] {
            assert!(!openable(&dir.join(bad)), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_links_leave_the_app() {
        let dev = Url::parse("http://localhost:1420").unwrap();
        let link = |s: &str| is_web_link(&Url::parse(s).unwrap(), Some(&dev));
        assert!(link("https://github.com/izz-linux/drovr/pull/12"));
        assert!(link("http://localhost:3000/"));
        assert!(link("mailto:izz@linux.com"));
        // drovr's own pages stay in drovr
        assert!(!link("http://localhost:1420/"));
        assert!(!link("tauri://localhost/"));
        assert!(!link(
            "asset://localhost/Users/izz/.claude/plans/report.html"
        ));
        assert!(!link("http://asset.localhost/report.html"));
        assert!(!link("http://tauri.localhost/"));
        assert!(!link("about:blank"));
    }
}
