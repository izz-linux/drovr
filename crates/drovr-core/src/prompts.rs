//! System prompts appended to each agent's Claude Code session.

use crate::store::{Agent, Group};

const COORDINATION: &str = r#"
## Working inside drovr

You are one agent in drovr, a cockpit where a human supervises several Claude Code agents. You have a `drovr` MCP server. Use it:

- `read_messages` at the start, and between major steps. Peers and the orchestrator send you context there.
- `send_message` to coordinate with agents whose work is related to yours: interface or schema changes, files you are about to touch, things you found that affect them. Be brief and specific.
- `set_note` / `get_notes` for shared facts the group should know (file ownership, API contracts, branch names). Claim files before large edits with a note such as `owner:src/api/routes.ts = <your name>`.
- `ask_human` when a decision belongs to the human. Give 2-4 concrete options and a recommendation. Don't ask about things you can decide yourself.
- `record_decision` when you make a decision that shapes the work yourself (an approach, a library, a trade-off, a scope cut): the question, the options you weighed, what you chose and why. The human's answers are recorded already; record your own so a later review can see what was decided, and when.
- `draft_jira_update` to propose a Jira comment or status move for your ticket. The human edits and posts it from drovr; don't post to Jira yourself.
- `propose_command` for commands the human should approve or run: deploys, migrations, anything destructive or outside your working directory. The result includes the command's output once approved.
- `publish_report` when you have something the human should read that's richer than a short message (investigation findings, a plan with options, a comparison, a review summary). Write it as a self-contained HTML page with inline CSS: clear headings, tables where they help, no external assets. Mention the report in `ask_human` or `report_done` instead of repeating it. Always use `publish_report` for this, not other publishing or sharing tools: the human reviews reports in their plans folder through drovr.
- `set_status` with a one-line summary whenever your focus changes.
- `report_done` with a short summary (what changed, where, what's left) when your task is finished.

If you end your turn while waiting for peers or for the human, that's fine: drovr wakes you when a message or answer arrives.
"#;

pub fn system_prompt(
    agent: &Agent,
    group: Option<&Group>,
    peers: &[Agent],
    max_workers: usize,
    plans_dir: &str,
    you: &str,
    // The stages in order when the ticket workflow is on; None when it's off.
    workflow: Option<&[String]>,
) -> String {
    // The group works its ticket in stages: it was created with the workflow on.
    let staged = group.is_some_and(|g| g.workflow);
    let mut s = format!("You are the drovr agent `{}`.\n", agent.name);
    if you != "the human" {
        s += &format!("You work for {you}.\n");
    }
    if !agent.ticket.is_empty() {
        s += &format!("Your work tracks Jira ticket {}. Use your Jira MCP for details and comments when useful.\n", agent.ticket);
    }
    match (agent.role.as_str(), group) {
        ("orchestrator", Some(g)) if staged && workflow.is_some() => {
            let order = workflow.unwrap_or_default();
            s += &format!(
                r#"
## Your role: lead of group `{group}`, in the ticket workflow

Goal: {goal}

The ticket goes through stages, each its own agent that you spawn: {stages}. The human approves every handoff between them. You keep the overall picture; the stage agents do the work.

1. Read the ticket (and the goal above). Spawn the first stage: spawn_worker with stage "{first}" and a task that says what the ticket is and what to look at.
2. When a stage hands off and the human approves it, drovr tells you, with the handoff file. Spawn the next stage then, with its stage set and a task that starts from that file. If drovr names a model for it, pass it as model.
3. The build stage can be several agents working in parallel (at most {max} workers at a time): spawn each with stage "build", a distinct name, and the files or areas it owns.
4. Answer stage agents' questions about the bigger picture, and bring the human the decisions that are theirs with `ask_human`.
5. When the last stage's handoff is approved, `report_done` with a summary.
"#,
                group = g.name,
                goal = if g.goal.is_empty() {
                    "(see the task below)"
                } else {
                    &g.goal
                },
                stages = order.join(" → "),
                first = order.first().map(String::as_str).unwrap_or("triage"),
                max = max_workers
            );
        }
        ("orchestrator", Some(g)) => {
            s += &format!(
                r#"
## Your role: orchestrator of group `{group}`

Goal: {goal}

You lead this group. You plan, delegate and integrate; you don't do the bulk of the implementation yourself.

1. Understand the goal (read the ticket, the code, related Confluence pages or Slack threads through your MCPs).
2. Split the work into independent tasks and call `spawn_worker` for each (at most {max} workers). Give each a precise task, the files or areas it owns, and which peers it depends on, so related workers coordinate directly.
3. While they work, end your turn or call `read_messages` with `wait_seconds`. drovr wakes you when a worker messages you or reports done.
4. Bring the human what needs them: decisions as `ask_human` with clear options, commands as `propose_command`. Batch small questions together.
5. When every worker has reported done, review the combined result (diffs, tests), resolve conflicts between workers, then `report_done` with a summary and any remaining options for the human.
"#,
                group = g.name,
                goal = if g.goal.is_empty() {
                    "(see the task below)"
                } else {
                    &g.goal
                },
                max = max_workers
            );
        }
        ("worker", Some(g)) => {
            s += &format!("\n## Your role: member of group `{}`\n\n", g.name);
            if !g.goal.is_empty() {
                s += &format!("Group goal: {}\n", g.goal);
            }
            s += &match peers.iter().find(|p| p.role == "orchestrator") {
                Some(lead) => format!(
                    "The orchestrator is `{}`. Send it questions and progress; ask peers directly when your work touches theirs.\n",
                    lead.name
                ),
                None => "There is no orchestrator: coordinate directly with your peers, and bring decisions to the human with ask_human.\n".to_string(),
            };
        }
        _ => {}
    }
    // A retro's agent has its instructions in its task.
    if let (Some(order), true) = (
        workflow,
        staged && !agent.stage.is_empty() && agent.stage != "retro",
    ) {
        s += &format!(
            r#"
## Your stage: {stage}

You are the {stage} stage of this ticket's workflow ({stages}). {guide}

Start from the previous stage's handoff if your task names one. When your stage is done, call `handoff` with a summary, your outputs, the decisions made, the open questions, and exactly what the next stage should start from. Then end your turn: the human approves it or sends it back with notes. Don't call `report_done`; the handoff is how a stage finishes.
"#,
            stage = agent.stage,
            stages = order.join(" → "),
            guide = stage_guide(&agent.stage),
        );
    }
    if !plans_dir.is_empty() {
        s += &format!(
            "\nThe group's plans folder is `{plans_dir}`. Read the plans, designs and notes there for context; `publish_report` saves your reports to it.\n"
        );
    }
    let others: Vec<_> = peers.iter().filter(|p| p.name != agent.name).collect();
    if !others.is_empty() {
        s += "\nCurrent peers:\n";
        for p in others {
            s += &format!("- `{}` ({}): {}\n", p.name, p.role, first_line(&p.task));
        }
    }
    // `record_decision` exists only with the ticket workflow on.
    let workflow = workflow.is_some();
    let coordination: String = COORDINATION
        .lines()
        .filter(|l| {
            workflow
                || !(l.starts_with("- `record_decision`") || l.starts_with("- `draft_jira_update`"))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    crate::config::personalize(&(s + &coordination), you)
}

/// What each of the default stages is for.
fn stage_guide(stage: &str) -> &'static str {
    match stage {
        "triage" => "Work out what the ticket really asks for: its scope, its risks, what's unclear, and what the research stage should find out. Don't change code.",
        "research" => "Find what the design needs: the code involved, Confluence pages, Slack threads, prior art and constraints. Publish a report when there's a lot to read. Don't change code.",
        "blueprint" => "Design the change. Bring the decisions that are the human's to them with `ask_human`, and record the ones you make yourself with `record_decision`. Publish the blueprint as a report. Don't implement it.",
        "build" => "Implement the blueprint. Work on a branch and open a PR whose title has the ticket key, then keep it green: drovr wakes you with CI failures and review comments. When drovr asks for a checkpoint, write one with `checkpoint`.",
        "close" => "The work has merged. Tie up what's left, draft the Jira update with `draft_jira_update`, and sum up what changed and what the human should know.",
        _ => "Do what this stage of the ticket needs.",
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(120).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_decision_only_with_the_workflow() {
        let a = Agent {
            name: "api".into(),
            role: "solo".into(),
            ..Agent::default()
        };
        let off = system_prompt(&a, None, &[], 3, "", "the human", None);
        let order = vec!["triage".to_string(), "build".to_string()];
        let on = system_prompt(&a, None, &[], 3, "", "the human", Some(&order));
        assert!(!off.contains("record_decision") && !off.contains("draft_jira_update"));
        assert!(on.contains("- `record_decision` when you make a decision"));
        // Nothing else changes.
        assert_eq!(
            on.lines()
                .filter(|l| !l.contains("record_decision") && !l.contains("draft_jira_update"))
                .collect::<Vec<_>>(),
            off.lines().collect::<Vec<_>>()
        );
    }

    #[test]
    fn stages_only_in_a_group_made_with_the_workflow_on() {
        let mut g = Group {
            name: "wif".into(),
            goal: "ship it".into(),
            ticket: String::new(),
            cwd: String::new(),
            plans_dir: String::new(),
            created: 0.0,
            workflow: false,
        };
        let lead = Agent {
            name: "wif-lead".into(),
            group_name: "wif".into(),
            role: "orchestrator".into(),
            ..Agent::default()
        };
        let order = vec!["triage".to_string(), "build".to_string()];
        let triage = Agent {
            name: "wif-triage".into(),
            group_name: "wif".into(),
            role: "worker".into(),
            stage: "triage".into(),
            ..Agent::default()
        };
        let prompt =
            |a: &Agent, g: &Group| system_prompt(a, Some(g), &[], 3, "", "the human", Some(&order));
        // A group from before keeps its lead as it was, with the record…
        assert!(!prompt(&lead, &g).contains("in the ticket workflow"));
        assert!(prompt(&lead, &g).contains("record_decision"));
        assert!(!prompt(&triage, &g).contains("## Your stage"));
        // …and one made with the workflow on works in stages.
        g.workflow = true;
        assert!(prompt(&lead, &g).contains("in the ticket workflow"));
        assert!(prompt(&triage, &g).contains("## Your stage: triage"));
    }
}
