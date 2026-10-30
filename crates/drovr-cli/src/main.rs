//! `drovr` CLI: the MCP server agents talk to, the run-finished hook, and
//! scriptable commands for herdr panes, plugins and shell use.

use anyhow::{bail, Result};
use drovr_core::engine::NewGroup;
use drovr_core::sources::{ticket_task, Sources};
use drovr_core::{Engine, NewAgent};
use serde_json::json;

const USAGE: &str = "drovr — orchestrate Claude Code agents

usage:
  drovr ls                                   list agents and their state
  drovr inbox                                pending decisions, commands, permissions
  drovr answer <id> <answer...>              answer a decision / allow|deny a permission
  drovr reject <id> [reason...]              reject an inbox item
  drovr run <id> [local|herdr]               run an approved command
  drovr ran <id> [output...]                 mark a command as run by you, with its output
  drovr new <name> <cwd> <task...>           start a solo agent
                                             (new, group and dispatch take --harness claude|codex)
  drovr adopt <session-id> <name> [--fork] [task...]
                                             start an agent from an existing Claude Code session
  drovr sessions                             list recent Claude Code sessions
  drovr group <name> <cwd> <goal...>         start a group with an orchestrator
  drovr dispatch <TICKET> [solo|group] [cwd] hand a Jira ticket to a new agent or a new group
  drovr dispatch <TICKET> to <group> [agent] hand it to an existing group (its lead by default)
  drovr prompt <agent> <text...>             send an agent a prompt
  drovr stop <agent> | resume <agent> [text] stop or resume an agent
  drovr rm <agent>                           stop and forget an agent
  drovr log <agent> [lines]                  an agent's output
  drovr attach <agent>                       print the command to open its session interactively
  drovr msgs [group]                         the message bus
  drovr ask <question...>                    ask an agent with your MCPs (Slack, Jira, …)
  drovr config                               where drovr reads its config and keeps its data
  drovr mcp --agent <name>                   (internal) MCP server for an agent
  drovr hook finished <agent>                (internal) called when a headless run exits";

fn rest(args: &[String], from: usize) -> String {
    args.get(from..).map(|a| a.join(" ")).unwrap_or_default()
}

fn arg<'a>(args: &'a [String], i: usize, what: &str) -> Result<&'a str> {
    match args.get(i) {
        Some(a) => Ok(a),
        None => bail!("missing {what}\n\n{USAGE}"),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("drovr: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--harness codex` anywhere: what runs the agents new, group and
    // dispatch start. Empty for defaults.harness.
    let harness = match args.iter().position(|a| a == "--harness") {
        Some(i) => {
            let name = arg(&args, i + 1, "harness name (claude or codex)")?.to_string();
            drovr_core::harness::named(&name)?;
            args.drain(i..=i + 1);
            name
        }
        None => String::new(),
    };
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let engine = Engine::open()?;
    match cmd {
        "mcp" if args.iter().any(|a| a == "--lookup") => {
            drovr_core::mcp::Server::lookup(engine).serve()?
        }
        "mcp" => {
            let name = match args.iter().position(|a| a == "--agent") {
                Some(i) => arg(&args, i + 1, "agent name")?.to_string(),
                None => std::env::var("DROVR_AGENT")?,
            };
            drovr_core::mcp::Server::new(engine, name).serve()?;
        }
        "hook" => match args.get(1).map(String::as_str) {
            Some("finished") => engine.finished(arg(&args, 2, "agent")?)?,
            _ => bail!("unknown hook"),
        },
        "ls" => {
            engine.refresh()?;
            engine.refresh_herdr()?;
            for a in engine.store.agents(None)? {
                println!(
                    "{:<28} {:<12} {:<9} {:<9} ${:<7.2} {}",
                    a.name,
                    if a.group_name.is_empty() {
                        "-"
                    } else {
                        &a.group_name
                    },
                    a.role,
                    engine.display_state(&a),
                    a.cost_usd,
                    a.status
                );
            }
        }
        "inbox" => {
            for i in engine.store.items(Some("pending"), 200)? {
                println!("#{:<4} {:<10} {:<24} {}", i.id, i.kind, i.agent, i.title);
                if !i.options.is_empty() {
                    println!("      options: {}", i.options.join(" | "));
                }
            }
        }
        "answer" => engine.answer(arg(&args, 1, "id")?.parse()?, &rest(&args, 2))?,
        "reject" => engine.reject(arg(&args, 1, "id")?.parse()?, &rest(&args, 2))?,
        "ran" => engine.mark_ran(arg(&args, 1, "id")?.parse()?, &rest(&args, 2))?,
        "run" => println!(
            "{}",
            engine.run_command(
                arg(&args, 1, "id")?.parse()?,
                args.get(2).map(String::as_str).unwrap_or("local"),
                None
            )?
        ),
        "new" => {
            let a = engine.start_agent(NewAgent {
                name: arg(&args, 1, "name")?.into(),
                cwd: arg(&args, 2, "cwd")?.into(),
                task: rest(&args, 3),
                harness: harness.clone(),
                ..NewAgent::default()
            })?;
            println!("started {} in {}", a.name, a.cwd);
        }
        "adopt" => {
            let fork = args.iter().any(|a| a == "--fork");
            let rest_args: Vec<String> = args
                .iter()
                .skip(3)
                .filter(|a| *a != "--fork")
                .cloned()
                .collect();
            let a = engine.start_agent(NewAgent {
                resume_session: arg(&args, 1, "session id")?.into(),
                name: arg(&args, 2, "name")?.into(),
                fork,
                task: rest_args.join(" "),
                ..NewAgent::default()
            })?;
            println!(
                "started {} from session {} in {}",
                a.name,
                arg(&args, 1, "session id")?,
                a.cwd
            );
        }
        "sessions" => {
            use drovr_core::harness::Harness;
            for s in drovr_core::harness::CLAUDE.recent_sessions(30) {
                println!("{}  {:<40}  {}", s.id, s.cwd, s.first_prompt);
            }
        }
        "group" => {
            let lead = engine.create_group(NewGroup {
                name: arg(&args, 1, "name")?.into(),
                cwd: arg(&args, 2, "cwd")?.into(),
                goal: rest(&args, 3),
                harness: harness.clone(),
                lead: true,
                ..NewGroup::default()
            })?;
            println!(
                "started group with orchestrator {}",
                lead.map(|a| a.name).unwrap_or_default()
            );
        }
        "dispatch" => {
            let key = arg(&args, 1, "ticket key")?;
            let mode = args.get(2).map(String::as_str).unwrap_or("solo");
            if mode == "to" {
                let group = arg(&args, 3, "group")?;
                let target = args.get(4).map(String::as_str).unwrap_or("");
                let ticket = Sources::new(&engine.cfg).jira_get(key)?;
                let a = engine.hand_off_to_group(
                    group,
                    &ticket.key,
                    &ticket_task(&ticket, "", &engine.cfg.your_name()),
                    target,
                )?;
                println!("{} → {} (group {group})", ticket.key, a.name);
                return Ok(());
            }
            let cwd = args.get(3).cloned().unwrap_or_default();
            let ticket = Sources::new(&engine.cfg).jira_get(key)?;
            let name = drovr_core::engine::slug(&ticket.key);
            let task = ticket_task(&ticket, "", &engine.cfg.your_name());
            let a = if mode == "group" {
                let spec = NewGroup {
                    name,
                    goal: task,
                    ticket: ticket.key.clone(),
                    cwd,
                    harness: harness.clone(),
                    lead: true,
                    ..NewGroup::default()
                };
                engine.create_group(spec)?.expect("lead requested")
            } else {
                engine.start_agent(NewAgent {
                    name,
                    cwd,
                    ticket: ticket.key.clone(),
                    task,
                    harness: harness.clone(),
                    ..NewAgent::default()
                })?
            };
            println!("{} → {}", ticket.key, a.name);
        }
        "prompt" => println!(
            "{}",
            engine.prompt_from_you(arg(&args, 1, "agent")?, &rest(&args, 2))?
        ),
        "stop" => engine.stop_by_you(arg(&args, 1, "agent")?)?,
        "resume" => println!(
            "{}",
            engine.resume_by_you(arg(&args, 1, "agent")?, &rest(&args, 2))?
        ),
        "rm" => {
            let note = engine.remove(arg(&args, 1, "agent")?)?;
            if !note.is_empty() {
                println!("{note}");
            }
        }
        "log" => {
            let n = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(200);
            for l in engine.output(arg(&args, 1, "agent")?, n)? {
                println!("[{}] {}", l.kind, l.text);
            }
        }
        "attach" => println!("{}", engine.attach_command(arg(&args, 1, "agent")?)?),
        "msgs" => {
            for m in engine
                .store
                .messages(args.get(1).map(String::as_str), 100)?
            {
                println!("{} → {}: {}", m.sender, m.recipient, m.body);
            }
        }
        "ask" => {
            let progress = |text: &str| eprintln!("… {text}");
            println!(
                "{}",
                Sources::with_progress(&engine.cfg, &progress).ask(&rest(&args, 1), None)?
            );
        }
        "config" => println!(
            "{}",
            json!({ "config": drovr_core::config::config_path(), "home": drovr_core::config::home() })
        ),
        "help" | "-h" | "--help" => println!("{USAGE}"),
        other => bail!("unknown command {other}\n\n{USAGE}"),
    }
    Ok(())
}
