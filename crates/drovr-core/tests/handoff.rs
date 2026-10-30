//! Handing a Jira ticket to an existing group.

use drovr_core::store::Store;
use drovr_core::{Config, Engine, NewAgent};

// Tests set the process-wide DROVR_HOME, so they take turns.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn setup(name: &str) -> (Engine, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("drovr-handoff-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    // A stand-in claude that just finishes.
    let claude = dir.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\ncat >/dev/null\necho '{\"type\":\"result\",\"is_error\":false}'\n",
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&claude)
        .status()
        .unwrap();
    std::env::set_var("DROVR_HOME", dir.join("home"));
    let mut cfg = Config::default();
    cfg.claude.command = claude.display().to_string();
    cfg.defaults.cwd = dir.join("proj").display().to_string();
    let store = Store::open(&dir.join("drovr.db")).unwrap();
    (Engine::with(store, cfg, dir.join("no-cli")), dir)
}

fn add(e: &Engine, name: &str, group: &str, role: &str, ticket: &str) {
    e.start_agent(NewAgent {
        name: name.into(),
        group: group.into(),
        role: role.into(),
        ticket: ticket.into(),
        task: "t".into(),
        ..NewAgent::default()
    })
    .unwrap();
}

#[test]
fn hands_tickets_to_existing_groups() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (e, dir) = setup("a");
    e.store.add_group("pay-api", "", "", "", "", false).unwrap();
    add(&e, "pay-api-lead", "pay-api", "orchestrator", "SRE-3230");
    add(&e, "b-ops-apps", "pay-api", "worker", "");
    std::thread::sleep(std::time::Duration::from_millis(300));
    for a in e.store.agents(None).unwrap() {
        e.store.set_agent(&a.name, "run_state", "idle").unwrap();
    }

    // Default: the lead, which keeps its old ticket and gains the new one.
    let a = e
        .hand_off_to_group("pay-api", "SRE-3253", "Upgrade to Bun 1.4", "")
        .unwrap();
    assert_eq!(a.name, "pay-api-lead");
    assert_eq!(a.ticket, "SRE-3230, SRE-3253");
    let prompt =
        std::fs::read_to_string(dir.join("home/agents/pay-api-lead/runs/2.prompt")).unwrap();
    assert!(
        prompt.contains("As the group's lead") && prompt.contains("Upgrade to Bun 1.4"),
        "{prompt}"
    );

    // A member chosen by name; handing the same ticket twice doesn't duplicate it.
    std::thread::sleep(std::time::Duration::from_millis(300));
    e.store
        .set_agent("b-ops-apps", "run_state", "idle")
        .unwrap();
    let b = e
        .hand_off_to_group("pay-api", "SRE-3253", "x", "b-ops-apps")
        .unwrap();
    assert_eq!(b.ticket, "SRE-3253");
    let again = e
        .hand_off_to_group("pay-api", "SRE-3253", "x", "b-ops-apps")
        .unwrap();
    assert_eq!(again.ticket, "SRE-3253");

    // Errors: unknown member, unknown group, no lead and nobody chosen.
    assert!(e
        .hand_off_to_group("pay-api", "SRE-1", "x", "nobody")
        .is_err());
    assert!(e.hand_off_to_group("nope", "SRE-1", "x", "").is_err());
    e.store
        .add_group("leaderless", "", "", "", "", false)
        .unwrap();
    let err = e
        .hand_off_to_group("leaderless", "SRE-1", "x", "")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no orchestrator"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn marks_commands_you_ran_yourself() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (mut e, dir) = setup("ran");
    e.cfg.defaults.your_name = "Izz".into();
    add(&e, "solo", "", "", "");
    std::thread::sleep(std::time::Duration::from_millis(300));
    e.store.set_agent("solo", "run_state", "idle").unwrap();
    let payload = serde_json::json!({ "command": "git push", "cwd": "/tmp" });
    let id = e
        .store
        .add_item("solo", "command", "Push", "", &[], &payload)
        .unwrap();

    e.mark_ran(id, "PR #482 opened").unwrap();
    let item = e.store.item(id).unwrap().unwrap();
    assert_eq!(item.status, "answered");
    assert!(
        item.answer.contains("Izz ran `git push` themselves"),
        "{}",
        item.answer
    );
    assert!(item.answer.contains("PR #482 opened"));
    // The idle agent woke up with it.
    let prompt = std::fs::read_to_string(dir.join("home/agents/solo/runs/2.prompt")).unwrap();
    assert!(prompt.contains("PR #482 opened"), "{prompt}");

    // Only commands can be marked as run.
    let q = e
        .store
        .add_item(
            "solo",
            "decision",
            "Which?",
            "",
            &[],
            &serde_json::json!({}),
        )
        .unwrap();
    assert!(e.mark_ran(q, "").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
