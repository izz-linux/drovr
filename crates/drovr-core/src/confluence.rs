//! Confluence pages agents write for you. An agent proposes a page (new
//! content for an existing page, or a new page under one); drovr reads the
//! live draft and puts the diff and the full copy in your Inbox. Approved, it
//! reads the page again and saves your copy as a Confluence draft, with
//! `confluence.page_update_cmd` or `page_create_cmd`. It never publishes.

use crate::config::Source;
use crate::engine::{capitalize, Engine};
use crate::shell::{self, extract_json};
use crate::store::{now, Agent, Item};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

/// A page or folder as `confluence.page_get_cmd` returned it.
#[derive(Debug, Default, PartialEq)]
pub struct Live {
    pub id: String,
    pub title: String,
    pub space: String,
    pub url: String,
    /// What an update must name: twg's `snapshotToken`.
    pub token: String,
    /// The draft's body, as HTML.
    pub body: String,
}

/// Read a page (or folder) by ID or URL.
pub fn get(src: &Source, page: &str) -> Result<Live> {
    let out = run(&src.page_get_cmd, &[("id", page)])?;
    let v = extract_json(&out)
        .ok_or_else(|| anyhow!("confluence.page_get_cmd didn't print JSON for {page}"))?;
    let live = live_of(&v);
    if live.id.is_empty() {
        bail!("confluence.page_get_cmd printed no page for {page}");
    }
    Ok(live)
}

/// Read twg's `content get` output: one page under `data` (or the first of
/// several), or the page itself.
pub fn live_of(v: &Value) -> Live {
    let d = match &v["data"] {
        Value::Object(_) => &v["data"],
        Value::Array(a) => a.first().unwrap_or(&Value::Null),
        _ => v,
    };
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    Live {
        id: text(&d["id"]),
        title: text(&d["title"]),
        space: text(&d["spaceId"]),
        url: [&d["externalUrl"], &d["url"], &d["webUrl"]]
            .iter()
            .map(|u| text(u))
            .find(|u| !u.is_empty())
            .unwrap_or_default(),
        token: text(&d["snapshotToken"]),
        body: body_of(d),
    }
}

/// The body: twg's `body.value`, or a string under `body`, or under
/// `html`/`content` inside `body` or `detail` objects. twg saves a large body
/// to a file instead and names it in `body.outputFile`.
fn body_of(d: &Value) -> String {
    fn inner(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => ["value", "html", "content", "body"]
                .iter()
                .find_map(|k| o.get(*k).and_then(inner))
                .or_else(|| {
                    let file = o.get("outputFile")?.as_str()?;
                    std::fs::read_to_string(file).ok()
                }),
            _ => None,
        }
    }
    d.get("body")
        .and_then(inner)
        .or_else(|| d.get("detail").filter(|v| v.is_object()).and_then(inner))
        .unwrap_or_default()
}

fn run(template: &str, vars: &[(&str, &str)]) -> Result<String> {
    if template.trim().is_empty() {
        bail!("publishing to Confluence isn't set up: set confluence.page_get_cmd, page_create_cmd and page_update_cmd");
    }
    let cmd = shell::fill(template, vars, &[]);
    shell::sh(&cmd, None, None, Duration::from_secs(120))?.into_stdout(&cmd)
}

/// A name no other diff or page copy in this process (or another) uses at
/// the same time: two in the same millisecond used to share one folder and
/// overwrite each other's files.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        (now() * 1000.0) as u64,
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A stable fingerprint of a page body (FNV-1a), to tell whether it changed.
fn fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// What a diff compares: Confluence's own node ids, indentation between
/// tags, and how quotes are escaped aren't changes you wrote.
fn normalized(html: &str) -> String {
    const ID: &str = " data-local-id=\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(ID) {
        out.push_str(&rest[..i]);
        let after = &rest[i + ID.len()..];
        rest = after.find('"').map_or("", |j| &after[j + 1..]);
    }
    out.push_str(rest);
    let out = out
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace(['\n', '\r', '\t'], " ");
    // Runs of spaces become one; between two tags, none.
    let mut s = String::with_capacity(out.len());
    let mut space = false;
    for c in out.chars() {
        if c == ' ' {
            space = true;
            continue;
        }
        if space && !(c == '<' && s.ends_with('>')) && !s.is_empty() {
            s.push(' ');
        }
        space = false;
        s.push(c);
    }
    s
}

/// One block of HTML per line, so a diff shows paragraphs and rows.
fn blocks(html: &str) -> String {
    let mut s = normalized(html);
    for tag in [
        "</p>",
        "</h1>",
        "</h2>",
        "</h3>",
        "</h4>",
        "</h5>",
        "</h6>",
        "</li>",
        "</tr>",
        "</table>",
        "</ul>",
        "</ol>",
        "</pre>",
        "</blockquote>",
        "</div>",
        "</figure>",
        "<br>",
        "<br/>",
        "<br />",
    ] {
        s = s.replace(tag, &format!("{tag}\n"));
    }
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l}\n"))
        .collect()
}

/// A unified diff of the live body against yours, block by block; empty when
/// they're the same.
pub fn diff(live: &str, yours: &str) -> Result<String> {
    let dir = std::env::temp_dir().join(format!("drovr-confluence-{}", unique()));
    std::fs::create_dir_all(&dir)?;
    let (a, b) = (dir.join("live.html"), dir.join("yours.html"));
    std::fs::write(&a, blocks(live))?;
    std::fs::write(&b, blocks(yours))?;
    let out = shell::sh(
        &format!(
            "git diff --no-index --no-color -U2 {} {}",
            shell::quote(&a.display().to_string()),
            shell::quote(&b.display().to_string())
        ),
        None,
        None,
        Duration::from_secs(30),
    );
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    // git exits 1 when the files differ.
    if out.code > 1 {
        bail!("git diff failed: {}", out.stderr.trim());
    }
    Ok(out
        .stdout
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .map(|l| format!("{l}\n"))
        .collect())
}

/// An agent's page, into your Inbox with the diff against the live draft.
pub fn propose(engine: &Engine, me: &Agent, a: &Value) -> Result<Value> {
    let src = &engine.cfg.confluence;
    let text = |k: &str| a[k].as_str().unwrap_or("").trim().to_string();
    let (title, body, page, parent) = (text("title"), text("body"), text("page"), text("parent"));
    if title.is_empty() || body.is_empty() {
        bail!("title and body are required");
    }
    let (action, target) = match (page.is_empty(), parent.is_empty()) {
        (false, true) => ("update", get(src, &page)?),
        (true, false) => ("create", get(src, &parent)?),
        _ => bail!("give page (an existing page to update) or parent (a page or folder to create the page under), not both"),
    };
    let cmd = if action == "update" {
        &src.page_update_cmd
    } else {
        &src.page_create_cmd
    };
    if cmd.trim().is_empty() {
        bail!("set confluence.page_{action}_cmd to {action} Confluence pages");
    }
    // The command reads your copy from a file: a page is too long for a
    // command line.
    let dir = crate::config::home().join("confluence");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}-{}.html", me.name, unique()));
    std::fs::write(&file, &body)?;
    // twg doesn't always give the page's URL; the one you gave will do.
    let given = if page.is_empty() { &parent } else { &page };
    let url = if target.url.is_empty() && given.starts_with("http") {
        given.clone()
    } else {
        target.url.clone()
    };
    let (changes, place) = match action {
        "update" => (
            diff(&target.body, &body)?,
            format!("New content for the draft of **{}**", target.title),
        ),
        _ => (
            String::new(),
            format!("A new page under **{}**", target.title),
        ),
    };
    let note = text("note");
    let item_body = format!(
        "{place}{}\n\nApproved, it's saved as a Confluence draft, not published.{}",
        if url.is_empty() {
            String::new()
        } else {
            format!(" ({url})")
        },
        if note.is_empty() {
            String::new()
        } else {
            format!("\n\n{note}")
        }
    );
    let id = engine.store.add_item(
        &me.name,
        "confluence",
        &format!("Confluence: {title}"),
        &item_body,
        &[],
        &json!({
            "action": action,
            "page": if action == "update" { &target.id } else { "" },
            "parent": if action == "create" { &target.id } else { "" },
            "space": target.space,
            "url": url,
            "title": title,
            "file": file.display().to_string(),
            "html": body,
            "diff": changes,
            "live": fingerprint(&target.body),
        }),
    )?;
    engine.record_item(id);
    Ok(json!(format!(
        "Proposed as Inbox item #{id}. {} reviews the diff against the live page and the full copy; if approved, it's saved as a Confluence draft (never published) and you're told. Carry on meanwhile.",
        capitalize(&engine.cfg.your_name())
    )))
}

/// Save an approved page as a draft. An update reads the page again first:
/// if it changed since the diff you reviewed, the item gets the new diff
/// and waits for you instead.
pub fn save(engine: &Engine, item: &Item) -> Result<String> {
    let src = &engine.cfg.confluence;
    let p = &item.payload;
    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
    let (title, file) = (s("title"), s("file"));
    if !Path::new(&file).is_file() {
        bail!("the page's copy ({file}) is gone; ask the agent to propose it again");
    }
    match s("action").as_str() {
        "update" => {
            let live = get(src, &s("page"))?;
            if fingerprint(&live.body) != s("live") {
                let mut fresh = p.clone();
                fresh["diff"] = json!(diff(&live.body, &s("html"))?);
                fresh["live"] = json!(fingerprint(&live.body));
                engine.store.set_item_payload(item.id, &fresh)?;
                bail!("the page changed in Confluence since this diff; the Inbox now shows the new one. Review it and approve again");
            }
            run(
                &src.page_update_cmd,
                &[
                    ("id", &live.id),
                    ("token", &live.token),
                    ("title", &title),
                    ("file", &file),
                ],
            )?;
            let url = if live.url.is_empty() {
                s("url")
            } else {
                live.url
            };
            Ok(format!(
                "Saved as a draft of \"{title}\" in Confluence, not published: {url}"
            ))
        }
        "create" => {
            let out = run(
                &src.page_create_cmd,
                &[
                    ("space", &s("space")),
                    ("parent", &s("parent")),
                    ("title", &title),
                    ("file", &file),
                ],
            )?;
            let new = extract_json(&out).map(|v| live_of(&v)).unwrap_or_default();
            let at = if new.url.is_empty() { new.id } else { new.url };
            Ok(format!(
                "Created \"{title}\" in Confluence as a draft, not published{}",
                if at.is_empty() {
                    String::new()
                } else {
                    format!(": {at}")
                }
            ))
        }
        other => bail!("unknown Confluence action {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_page_and_diffs_by_block() {
        let v = json!({ "apiVersion": "v2", "command": "confluence.content.get", "data": {
            "id": "123", "type": "page", "title": "Runbook", "status": "draft", "spaceId": 98,
            "externalUrl": "https://x.atlassian.net/wiki/spaces/SRE/pages/123", "snapshotToken": "tok",
            "body": "<p>One</p><p>Two</p>" } });
        let live = live_of(&v);
        assert_eq!(
            (
                live.id.as_str(),
                live.space.as_str(),
                live.token.as_str(),
                live.body.as_str()
            ),
            ("123", "98", "tok", "<p>One</p><p>Two</p>")
        );
        // The body may sit inside an object.
        let nested =
            json!({ "data": [{ "id": "7", "detail": { "body": { "value": "<p>x</p>" } } }] });
        assert_eq!(live_of(&nested).body, "<p>x</p>");
        assert_eq!(
            live_of(&json!({ "data": { "id": "7", "detail": "full" } })).body,
            ""
        );
        // twg's shape, with the body inline or saved to a file.
        let real = json!({ "apiVersion": "v2", "command": "confluence.content.get", "data": {
            "id": "5282234374", "type": "page", "title": "Runbook", "status": "draft", "spaceId": "2928148605",
            "snapshotToken": "d1:325", "detail": "full", "body": { "format": "html", "value": "<p data-local-id=\"a1\">Hi</p>" } } });
        let live = live_of(&real);
        assert_eq!(
            (live.token.as_str(), live.body.as_str()),
            ("d1:325", "<p data-local-id=\"a1\">Hi</p>")
        );
        let file = std::env::temp_dir().join(format!("drovr-body-{}.html", std::process::id()));
        std::fs::write(&file, "<p>Saved</p>").unwrap();
        let saved = json!({ "data": { "id": "1", "body": { "format": "html", "outputFile": file.display().to_string() } } });
        assert_eq!(live_of(&saved).body, "<p>Saved</p>");
        let _ = std::fs::remove_file(&file);
        // Confluence's ids, indentation and escaping aren't changes.
        let confluence = "<table data-local-id=\"t\">\n  <tr data-local-id=\"r\">\n    <td data-local-id=\"c\">\n      <p data-local-id=\"p\">Don&#39;t stop</p>\n    </td>\n  </tr>\n</table>";
        assert_eq!(
            diff(
                confluence,
                "<table><tr><td><p>Don't stop</p></td></tr></table>"
            )
            .unwrap(),
            ""
        );
        assert!(diff(
            confluence,
            "<table><tr><td><p>Do stop</p></td></tr></table>"
        )
        .unwrap()
        .contains("+<table><tr><td><p>Do stop</p>"));
        // One changed paragraph shows as one line out, one in.
        let d = diff("<p>One</p><p>Two</p>", "<p>One</p><p>Three</p>").unwrap();
        assert!(
            d.contains("-<p>Two</p>\n") && d.contains("+<p>Three</p>\n"),
            "{d}"
        );
        assert_eq!(diff("<p>Same</p>", "<p>Same</p>\n").unwrap(), "");
        assert_ne!(fingerprint("a"), fingerprint("b"));
    }

    #[test]
    fn diffs_at_the_same_time_keep_to_their_own_files() {
        let threads: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..25 {
                        let d = diff("<p>old</p>", &format!("<p>{t}-{i}</p>")).unwrap();
                        assert!(d.contains(&format!("+<p>{t}-{i}</p>")), "{t}-{i}: {d:?}");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn proposes_reviews_and_saves_drafts_only() {
        use crate::config::Config;
        use crate::store::Store;
        let dir =
            std::env::temp_dir().join(format!("drovr-confluence-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.display().to_string();
        let page = |id: &str, body: &str| {
            std::fs::write(
                dir.join(format!("{id}.json")),
                json!({ "data": { "id": id, "title": format!("Page {id}"), "spaceId": "98", "snapshotToken": format!("tok-{body}"),
                    "externalUrl": format!("https://x/wiki/{id}"), "body": body } }).to_string(),
            )
            .unwrap();
        };
        page("123", "<p>Old</p>");
        page("900", "");
        // Stand-ins for twg: read a page, update a draft, create a draft page.
        let mut cfg = Config::default();
        cfg.defaults.auto_wake = false;
        cfg.reports.dir = format!("{d}/plans");
        cfg.confluence.page_get_cmd = format!("cat {d}/$(basename {{id}}).json");
        cfg.confluence.page_update_cmd = format!(
            "echo update {{id}} {{token}} {{title}} >> {d}/log; cp {{file}} {d}/saved.html"
        );
        cfg.confluence.page_create_cmd = format!("echo create {{space}} {{parent}} {{title}} >> {d}/log; echo '{{\"data\":{{\"id\":\"555\",\"externalUrl\":\"https://x/wiki/555\"}}}}'");
        let engine = Engine::with(Store::open(&dir.join("t.db")).unwrap(), cfg, "drovr".into());
        let me = Agent {
            name: "writer".into(),
            ..Agent::default()
        };
        engine.store.add_agent(&me).unwrap();
        let body = "<h2>Why</h2><p>New</p>";

        // Update: the Inbox item carries the diff against the live draft.
        let ask = |a: Value| propose(&engine, &me, &a);
        assert!(ask(json!({ "title": "T", "body": body })).is_err());
        assert!(
            ask(json!({ "title": "T", "body": body, "page": "123", "parent": "900" })).is_err()
        );
        ask(json!({ "title": "Runbook", "body": body, "page": "https://x/wiki/123" })).unwrap();
        let pending = || engine.store.items(Some("pending"), 10).unwrap();
        let item = pending()
            .into_iter()
            .find(|i| i.kind == "confluence")
            .unwrap();
        let diff = item.payload["diff"].as_str().unwrap();
        assert!(
            diff.contains("-<p>Old</p>") && diff.contains("+<p>New</p>"),
            "{diff}"
        );
        // Someone edits the page before you approve: no save, a new diff.
        page("123", "<p>Edited</p>");
        assert!(engine.answer(item.id, "approve").is_err());
        let item = engine.store.item(item.id).unwrap().unwrap();
        assert_eq!(item.status, "pending");
        assert!(item.payload["diff"]
            .as_str()
            .unwrap()
            .contains("-<p>Edited</p>"));
        assert!(!dir.join("log").exists());
        // Approved as it now stands: saved as a draft with the fresh token.
        engine.answer(item.id, "approve").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("saved.html")).unwrap(),
            body
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("log")).unwrap(),
            "update 123 tok-<p>Edited</p> Runbook\n"
        );
        assert!(engine
            .store
            .item(item.id)
            .unwrap()
            .unwrap()
            .answer
            .contains("not published: https://x/wiki/123"));
        // The agent hears how it went.
        let told = engine.store.undelivered_answers("writer").unwrap();
        assert!(told.iter().any(|i| i.id == item.id));
        assert!(crate::engine::answer_text(&told[0], "Izz").contains("Saved as a draft"));

        // Create: a new page under a folder, in its space.
        ask(json!({ "title": "Notes", "body": body, "parent": "900" })).unwrap();
        let item = pending()
            .into_iter()
            .find(|i| i.kind == "confluence")
            .unwrap();
        engine.answer(item.id, "approve").unwrap();
        assert!(std::fs::read_to_string(dir.join("log"))
            .unwrap()
            .ends_with("create 98 900 Notes\n"));
        assert!(engine
            .store
            .item(item.id)
            .unwrap()
            .unwrap()
            .answer
            .ends_with("https://x/wiki/555"));
        for i in engine.store.items(None, 10).unwrap() {
            let _ = std::fs::remove_file(i.payload["file"].as_str().unwrap_or(""));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
