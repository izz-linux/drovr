//! Shared SQLite state.
//!
//! The desktop app, the CLI and every agent's drovr MCP server open the same
//! database, so agents coordinate through it even when the app is closed.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS groups (
    name TEXT PRIMARY KEY,
    goal TEXT NOT NULL DEFAULT '',
    ticket TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    plans_dir TEXT NOT NULL DEFAULT '',
    created REAL NOT NULL,
    workflow INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS agents (
    name TEXT PRIMARY KEY,
    group_name TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL DEFAULT 'solo',
    backend TEXT NOT NULL DEFAULT 'headless',
    harness TEXT NOT NULL DEFAULT 'claude',
    run_state TEXT NOT NULL DEFAULT 'idle',
    status TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    session_id TEXT NOT NULL DEFAULT '',
    pane TEXT NOT NULL DEFAULT '',
    ticket TEXT NOT NULL DEFAULT '',
    task TEXT NOT NULL DEFAULT '',
    pid INTEGER NOT NULL DEFAULT 0,
    runs INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0,
    created REAL NOT NULL,
    updated REAL NOT NULL,
    stage TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    sender TEXT NOT NULL,
    recipient TEXT NOT NULL,
    group_name TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS cursors (
    agent TEXT PRIMARY KEY,
    last_message INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS inbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    agent TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    options TEXT NOT NULL DEFAULT '[]',
    payload TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    answer TEXT NOT NULL DEFAULT '',
    delivered INTEGER NOT NULL DEFAULT 0,
    answered_ts REAL
);
CREATE TABLE IF NOT EXISTS report_meta (
    path TEXT PRIMARY KEY,
    ts REAL NOT NULL,
    agent TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    seen INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS notes (
    group_name TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    author TEXT NOT NULL,
    ts REAL NOT NULL,
    PRIMARY KEY (group_name, key)
);
"#;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub goal: String,
    pub ticket: String,
    pub cwd: String,
    /// Subfolder of the plans folder for this group's reports and prompts.
    /// Empty means the group's name.
    pub plans_dir: String,
    pub created: f64,
    /// Created with the ticket workflow on: its lead runs the ticket in
    /// stages. Groups from before (or made with it off) work as they did.
    pub workflow: bool,
}

impl Group {
    pub fn plans_folder(&self) -> &str {
        if self.plans_dir.is_empty() {
            &self.name
        } else {
            &self.plans_dir
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Agent {
    pub name: String,
    pub group_name: String,
    /// solo | orchestrator | worker
    pub role: String,
    /// headless | herdr
    pub backend: String,
    /// The CLI that runs it: claude | codex (see `harness`).
    pub harness: String,
    /// working | idle | done | error | stopped
    pub run_state: String,
    /// One-line status the agent reports about itself.
    pub status: String,
    pub cwd: String,
    pub session_id: String,
    pub pane: String,
    pub ticket: String,
    pub task: String,
    pub pid: i64,
    pub runs: i64,
    pub cost_usd: f64,
    pub created: f64,
    pub updated: f64,
    /// Its stage in the ticket workflow (`triage`, `build`, …); empty for
    /// agents outside it.
    pub stage: String,
    /// The model it runs on; empty for the harness's configured default.
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub ts: f64,
    pub sender: String,
    /// An agent name, `@group`, or `*`.
    pub recipient: String,
    pub group_name: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: i64,
    pub ts: f64,
    pub agent: String,
    /// decision | command | permission | spawn | done
    pub kind: String,
    pub title: String,
    pub body: String,
    pub options: Vec<String>,
    pub payload: Value,
    /// pending | answered | rejected | dismissed
    pub status: String,
    pub answer: String,
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub key: String,
    pub value: String,
    pub author: String,
    pub ts: f64,
}

/// What drovr knows about an HTML file in the plans folder: who published
/// it and whether the human has opened it. Files nobody published have none.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportMeta {
    pub path: String,
    pub ts: f64,
    pub agent: String,
    /// The title it was published under; empty for reports from before titles were kept.
    pub title: String,
    pub summary: String,
    pub seen: bool,
}

pub struct Store {
    db: Connection,
}

fn agent_row(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        name: r.get("name")?,
        group_name: r.get("group_name")?,
        role: r.get("role")?,
        backend: r.get("backend")?,
        harness: r.get("harness")?,
        run_state: r.get("run_state")?,
        status: r.get("status")?,
        cwd: r.get("cwd")?,
        session_id: r.get("session_id")?,
        pane: r.get("pane")?,
        ticket: r.get("ticket")?,
        task: r.get("task")?,
        pid: r.get("pid")?,
        runs: r.get("runs")?,
        cost_usd: r.get("cost_usd")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
        stage: r.get("stage")?,
        model: r.get("model")?,
    })
}

fn item_row(r: &Row) -> rusqlite::Result<Item> {
    let options: String = r.get("options")?;
    let payload: String = r.get("payload")?;
    Ok(Item {
        id: r.get("id")?,
        ts: r.get("ts")?,
        agent: r.get("agent")?,
        kind: r.get("kind")?,
        title: r.get("title")?,
        body: r.get("body")?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        status: r.get("status")?,
        answer: r.get("answer")?,
        delivered: r.get::<_, i64>("delivered")? != 0,
    })
}

fn message_row(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get("id")?,
        ts: r.get("ts")?,
        sender: r.get("sender")?,
        recipient: r.get("recipient")?,
        group_name: r.get("group_name")?,
        body: r.get("body")?,
    })
}

impl Store {
    pub fn open_default() -> Result<Store> {
        Store::open(&crate::config::home().join("drovr.db"))
    }

    pub fn open(path: &Path) -> Result<Store> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(30))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(SCHEMA)?;
        // Databases created before groups had a plans folder.
        let has_plans_dir = db
            .prepare("SELECT 1 FROM pragma_table_info('groups') WHERE name = 'plans_dir'")?
            .exists([])?;
        if !has_plans_dir {
            db.execute_batch("ALTER TABLE groups ADD COLUMN plans_dir TEXT NOT NULL DEFAULT ''")?;
        }
        // Databases from before groups were marked for the ticket workflow.
        let has_workflow = db
            .prepare("SELECT 1 FROM pragma_table_info('groups') WHERE name = 'workflow'")?
            .exists([])?;
        if !has_workflow {
            db.execute_batch("ALTER TABLE groups ADD COLUMN workflow INTEGER NOT NULL DEFAULT 0")?;
        }
        // Databases from before harnesses: all their agents ran Claude Code.
        let has_harness = db
            .prepare("SELECT 1 FROM pragma_table_info('agents') WHERE name = 'harness'")?
            .exists([])?;
        if !has_harness {
            db.execute_batch(
                "ALTER TABLE agents ADD COLUMN harness TEXT NOT NULL DEFAULT 'claude'",
            )?;
        }
        // Databases from before stages and per-agent models.
        for column in ["stage", "model"] {
            let has = db
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('agents') WHERE name = '{column}'"
                ))?
                .exists([])?;
            if !has {
                db.execute_batch(&format!(
                    "ALTER TABLE agents ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
                ))?;
            }
        }
        // Databases from before republishing a report replaced it.
        let has_title = db
            .prepare("SELECT 1 FROM pragma_table_info('report_meta') WHERE name = 'title'")?
            .exists([])?;
        if !has_title {
            db.execute_batch("ALTER TABLE report_meta ADD COLUMN title TEXT NOT NULL DEFAULT ''")?;
        }
        Ok(Store { db })
    }

    // -- groups ---------------------------------------------------------------

    pub fn add_group(
        &self,
        name: &str,
        goal: &str,
        ticket: &str,
        cwd: &str,
        plans_dir: &str,
        workflow: bool,
    ) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO groups (name, goal, ticket, cwd, plans_dir, created, workflow) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![name, goal, ticket, cwd, plans_dir, now(), workflow],
        )?;
        Ok(())
    }

    pub fn groups(&self) -> Result<Vec<Group>> {
        let mut stmt = self.db.prepare(
            "SELECT name, goal, ticket, cwd, plans_dir, created, workflow FROM groups ORDER BY created",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Group {
                name: r.get(0)?,
                goal: r.get(1)?,
                ticket: r.get(2)?,
                cwd: r.get(3)?,
                plans_dir: r.get(4)?,
                created: r.get(5)?,
                workflow: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn group(&self, name: &str) -> Result<Option<Group>> {
        Ok(self.groups()?.into_iter().find(|g| g.name == name))
    }

    pub fn set_group_ticket(&self, name: &str, ticket: &str) -> Result<()> {
        self.db.execute(
            "UPDATE groups SET ticket = ?1 WHERE name = ?2",
            params![ticket, name],
        )?;
        Ok(())
    }

    pub fn remove_group(&self, name: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM groups WHERE name = ?1", [name])?;
        Ok(())
    }

    // -- agents ---------------------------------------------------------------

    pub fn add_agent(&self, a: &Agent) -> Result<()> {
        let t = now();
        self.db.execute(
            "INSERT INTO agents (name, group_name, role, backend, harness, run_state, status, cwd, session_id, pane, \
             ticket, task, pid, runs, cost_usd, created, updated, stage, model) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 0, 0, ?13, ?13, ?14, ?15)",
            params![
                a.name, a.group_name, a.role, a.backend, a.harness, a.run_state, a.status, a.cwd, a.session_id, a.pane,
                a.ticket, a.task, t, a.stage, a.model
            ],
        )?;
        Ok(())
    }

    pub fn agent(&self, name: &str) -> Result<Option<Agent>> {
        Ok(self
            .db
            .query_row("SELECT * FROM agents WHERE name = ?1", [name], agent_row)
            .optional()?)
    }

    pub fn agents(&self, group: Option<&str>) -> Result<Vec<Agent>> {
        let mut stmt = self.db.prepare(
            "SELECT * FROM agents WHERE (?1 IS NULL OR group_name = ?1) \
             ORDER BY group_name, role = 'orchestrator' DESC, created",
        )?;
        let rows = stmt.query_map([group], agent_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set a text column on an agent. `column` is always a literal from our code.
    pub fn set_agent(&self, name: &str, column: &str, value: &str) -> Result<()> {
        self.db.execute(
            &format!("UPDATE agents SET {column} = ?1, updated = ?2 WHERE name = ?3"),
            params![value, now(), name],
        )?;
        Ok(())
    }

    pub fn set_pid(&self, name: &str, pid: i64) -> Result<()> {
        self.db.execute(
            "UPDATE agents SET pid = ?1, updated = ?2 WHERE name = ?3",
            params![pid, now(), name],
        )?;
        Ok(())
    }

    pub fn set_cost(&self, name: &str, usd: f64) -> Result<()> {
        self.db.execute(
            "UPDATE agents SET cost_usd = ?1 WHERE name = ?2",
            params![usd, name],
        )?;
        Ok(())
    }

    /// Atomically mark an agent as working and bump its run counter. Returns
    /// the new run number, or None when it was already working.
    pub fn claim_run(&self, name: &str) -> Result<Option<i64>> {
        let n = self.db.execute(
            "UPDATE agents SET run_state = 'working', runs = runs + 1, updated = ?1 \
             WHERE name = ?2 AND run_state != 'working'",
            params![now(), name],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(self.agent(name)?.map(|a| a.runs))
    }

    pub fn remove_agent(&self, name: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM agents WHERE name = ?1", [name])?;
        self.db
            .execute("DELETE FROM cursors WHERE agent = ?1", [name])?;
        // Nothing is left to answer them.
        self.db.execute(
            "UPDATE inbox SET status = 'dismissed', answered_ts = ?2 WHERE agent = ?1 AND status = 'pending'",
            params![name, now()],
        )?;
        Ok(())
    }

    // -- messages -------------------------------------------------------------

    pub fn send(&self, sender: &str, recipient: &str, body: &str, group: &str) -> Result<i64> {
        self.db.execute(
            "INSERT INTO messages (ts, sender, recipient, group_name, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now(), sender, recipient, group, body],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Messages addressed to `agent` (directly, to its group, or to everyone)
    /// that it hasn't read yet.
    pub fn unread(&self, agent: &str, group: &str) -> Result<Vec<Message>> {
        let last: i64 = self
            .db
            .query_row(
                "SELECT last_message FROM cursors WHERE agent = ?1",
                [agent],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut stmt = self.db.prepare(
            "SELECT * FROM messages WHERE id > ?1 AND sender != ?2 AND \
             (recipient = ?2 OR recipient = '*' OR (recipient = '@group' AND group_name = ?3 AND ?3 != '')) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![last, agent, group], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Your messages to `agent` that it hasn't read yet: the ones waiting for
    /// its run to end.
    pub fn queued(&self, agent: &str) -> Result<Vec<Message>> {
        let mut stmt = self.db.prepare(
            "SELECT * FROM messages WHERE sender = 'human' AND recipient = ?1 AND \
             id > COALESCE((SELECT last_message FROM cursors WHERE agent = ?1), 0) ORDER BY id",
        )?;
        let rows = stmt.query_map([agent], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Take one of your queued messages back before `agent` reads it.
    pub fn unqueue(&self, agent: &str, id: i64) -> Result<bool> {
        let n = self.db.execute(
            "DELETE FROM messages WHERE id = ?1 AND sender = 'human' AND recipient = ?2 AND \
             id > COALESCE((SELECT last_message FROM cursors WHERE agent = ?2), 0)",
            params![id, agent],
        )?;
        Ok(n > 0)
    }

    pub fn mark_read(&self, agent: &str, upto: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO cursors (agent, last_message) VALUES (?1, ?2) \
             ON CONFLICT(agent) DO UPDATE SET last_message = MAX(last_message, excluded.last_message)",
            params![agent, upto],
        )?;
        Ok(())
    }

    pub fn messages(&self, group: Option<&str>, limit: i64) -> Result<Vec<Message>> {
        let mut stmt = self.db.prepare(
            "SELECT * FROM (SELECT * FROM messages WHERE (?1 IS NULL OR group_name = ?1) ORDER BY id DESC LIMIT ?2) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![group, limit], message_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -- inbox ----------------------------------------------------------------

    pub fn add_item(
        &self,
        agent: &str,
        kind: &str,
        title: &str,
        body: &str,
        options: &[String],
        payload: &Value,
    ) -> Result<i64> {
        self.db.execute(
            "INSERT INTO inbox (ts, agent, kind, title, body, options, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![now(), agent, kind, title, body, serde_json::to_string(options)?, payload.to_string()],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn set_item_payload(&self, id: i64, payload: &Value) -> Result<()> {
        self.db.execute(
            "UPDATE inbox SET payload = ?1 WHERE id = ?2",
            params![payload.to_string(), id],
        )?;
        Ok(())
    }

    pub fn item(&self, id: i64) -> Result<Option<Item>> {
        Ok(self
            .db
            .query_row("SELECT * FROM inbox WHERE id = ?1", [id], item_row)
            .optional()?)
    }

    pub fn items(&self, status: Option<&str>, limit: i64) -> Result<Vec<Item>> {
        let mut stmt = self.db.prepare(
            "SELECT * FROM (SELECT * FROM inbox WHERE (?1 IS NULL OR status = ?1) ORDER BY id DESC LIMIT ?2) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![status, limit], item_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn pending_count(&self, agent: &str) -> Result<i64> {
        Ok(self.db.query_row(
            "SELECT COUNT(*) FROM inbox WHERE agent = ?1 AND status = 'pending' AND kind != 'done'",
            [agent],
            |r| r.get(0),
        )?)
    }

    /// Resolve a pending item. False if it was already resolved.
    pub fn resolve_item(&self, id: i64, status: &str, answer: &str) -> Result<bool> {
        let n = self.db.execute(
            "UPDATE inbox SET status = ?1, answer = ?2, answered_ts = ?3 WHERE id = ?4 AND status = 'pending'",
            params![status, answer, now(), id],
        )?;
        Ok(n == 1)
    }

    pub fn mark_delivered(&self, id: i64) -> Result<()> {
        self.db
            .execute("UPDATE inbox SET delivered = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Answers to an agent's questions and commands that it hasn't seen yet.
    pub fn undelivered_answers(&self, agent: &str) -> Result<Vec<Item>> {
        let mut stmt = self.db.prepare(
            "SELECT * FROM inbox WHERE agent = ?1 AND status IN ('answered', 'rejected') AND delivered = 0 \
             AND kind IN ('decision', 'command', 'jira', 'confluence') ORDER BY id",
        )?;
        let rows = stmt.query_map([agent], item_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -- reports --------------------------------------------------------------

    /// Record that an agent published `path`; it shows as unread until opened.
    pub fn publish_report(
        &self,
        path: &str,
        agent: &str,
        title: &str,
        summary: &str,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO report_meta (path, ts, agent, title, summary, seen) VALUES (?1, ?2, ?3, ?4, ?5, 0) \
             ON CONFLICT(path) DO UPDATE SET ts = excluded.ts, agent = excluded.agent, \
             title = excluded.title, summary = excluded.summary, seen = 0",
            params![path, now(), agent, title, summary],
        )?;
        Ok(())
    }

    pub fn report_meta(&self) -> Result<Vec<ReportMeta>> {
        let mut stmt = self
            .db
            .prepare("SELECT path, ts, agent, title, summary, seen FROM report_meta")?;
        let rows = stmt.query_map([], |r| {
            Ok(ReportMeta {
                path: r.get(0)?,
                ts: r.get(1)?,
                agent: r.get(2)?,
                title: r.get(3)?,
                summary: r.get(4)?,
                seen: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unseen_reports(&self) -> Result<i64> {
        Ok(self
            .db
            .query_row("SELECT COUNT(*) FROM report_meta WHERE seen = 0", [], |r| {
                r.get(0)
            })?)
    }

    pub fn mark_report_seen(&self, path: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO report_meta (path, ts, seen) VALUES (?1, ?2, 1) ON CONFLICT(path) DO UPDATE SET seen = 1",
            params![path, now()],
        )?;
        Ok(())
    }

    pub fn mark_all_reports_seen(&self) -> Result<()> {
        self.db
            .execute("UPDATE report_meta SET seen = 1 WHERE seen = 0", [])?;
        Ok(())
    }

    // -- notes (a per-group blackboard) ---------------------------------------

    pub fn set_note(&self, group: &str, key: &str, value: &str, author: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO notes (group_name, key, value, author, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![group, key, value, author, now()],
        )?;
        Ok(())
    }

    pub fn notes(&self, group: &str) -> Result<Vec<Note>> {
        let mut stmt = self.db.prepare(
            "SELECT key, value, author, ts FROM notes WHERE group_name = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map([group], |r| {
            Ok(Note {
                key: r.get(0)?,
                value: r.get(1)?,
                author: r.get(2)?,
                ts: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_messages_until_read() {
        let dir = std::env::temp_dir().join(format!("drovr-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let st = Store::open(&dir.join("t.db")).unwrap();
        let a = st.send("human", "api", "first", "g").unwrap();
        st.send("tests", "api", "from a peer", "g").unwrap();
        let b = st.send("human", "api", "second", "g").unwrap();
        st.send("human", "tests", "not for api", "g").unwrap();
        let bodies = |v: Vec<Message>| v.into_iter().map(|m| m.body).collect::<Vec<_>>();
        assert_eq!(bodies(st.queued("api").unwrap()), ["first", "second"]);

        // Taken back before it's read; another agent's can't be.
        assert!(st.unqueue("api", b).unwrap());
        assert!(!st.unqueue("tests", a).unwrap());
        assert_eq!(bodies(st.queued("api").unwrap()), ["first"]);

        // Once read it's no longer queued, and can't be taken back.
        st.mark_read("api", a).unwrap();
        assert!(st.queued("api").unwrap().is_empty());
        assert!(!st.unqueue("api", a).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
