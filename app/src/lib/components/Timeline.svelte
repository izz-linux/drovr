<script lang="ts">
  import { api, type RecordEvent, type TicketRecord } from "#lib/api.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import Modal from "./Modal.svelte";

  // A ticket's record: every run, question, answer, decision, message and
  // wake, oldest first, from the ledger.jsonl files in the plans folder.
  let records = $state<TicketRecord[]>([]);
  let events = $state<RecordEvent[]>([]);
  let loaded = $state(false);
  let filter = $state("all");
  let open = $state<Record<number, boolean>>({});

  const ticket = $derived(app.timeline);

  $effect(() => {
    const t = ticket;
    if (!t) return;
    let alive = true;
    loaded = false;
    open = {};
    const load = async () => {
      try {
        const [r, e] = await Promise.all([api.records(), api.timeline(t)]);
        if (!alive) return;
        records = r;
        events = e;
        loaded = true;
      } catch (err) { toast(String(err)); }
    };
    load();
    const timer = setInterval(load, 5000);
    return () => { alive = false; clearInterval(timer); };
  });

  // What each filter shows.
  const FILTERS: [string, string, (k: string) => boolean][] = [
    ["all", "Everything", () => true],
    ["runs", "Runs", (k) => k.startsWith("run_") || k === "herdr_prompt"],
    ["decisions", "Decisions & Inbox", (k) => ["asked", "answered", "decision"].includes(k)],
    ["talk", "Messages & prompts", (k) => ["message", "you_prompt", "send_now", "unqueued"].includes(k)],
    ["work", "Status, notes & reports", (k) => ["status", "note", "report", "ticket_set"].includes(k)],
    ["agents", "Agents & wakes", (k) => ["agent_started", "agent_removed", "wake", "wake_paused", "stopped", "resumed"].includes(k)],
    ["outside", "Jira & PRs", (k) => k.startsWith("jira_") || k.startsWith("pr_") || k.startsWith("sprint_")],
    ["stages", "Stages", (k) => k.startsWith("handoff") || k.startsWith("checkpoint") || k === "rehydrated"],
    ["retro", "Retro", (k) => k.startsWith("retro") || k.startsWith("proposal")],
  ];
  const shown = $derived(events.filter((e) => (FILTERS.find((f) => f[0] === filter)?.[2] ?? (() => true))(e.kind)));

  const totals = $derived.by(() => {
    const n = (k: string) => events.filter((e) => e.kind === k).length;
    const answered = events.filter((e) => e.kind === "answered");
    return {
      runs: n("run_end"),
      cost: events.reduce((s, e) => s + (e.kind === "run_end" ? Number(e.cost_usd) || 0 : 0), 0),
      asked: n("asked"),
      declined: answered.filter((e) => e.status === "rejected" || e.status === "dismissed").length,
      decisions: n("decision") + answered.filter((e) => e.type === "decision").length,
      wakes: n("wake"),
      agents: new Set(events.map((e) => e.agent).filter(Boolean)).size,
    };
  });

  const ITEM: Record<string, string> = { decision: "Question", command: "Command", permission: "Permission", spawn: "Spawn worker", done: "Done", watch: "Watcher asked", jira: "Jira draft" };
  const first = (s: unknown, max = 160) => {
    const line = String(s ?? "").trim().split("\n")[0];
    return line.length > max ? line.slice(0, max) + "…" : line;
  };
  const tokens = (t: any) => (t && (t.input != null || t.output != null) ? ` · ${t.input ?? 0} in, ${t.output ?? 0} out` : "");

  // One line for each kind of event; the rest of it is under the row.
  function label(e: RecordEvent): [string, string] {
    switch (e.kind) {
      case "agent_started": return ["Started", `${e.role ?? ""}${e.stage ? ` · ${e.stage} stage` : ""} · ${e.harness ?? ""}${e.model ? ` · ${e.model}` : ""} — ${first(e.task)}`];
      case "handoff": return [`Handoff: ${e.stage}`, `→ ${e.next ?? "done"} · ${first(e.summary, 120)}`];
      case "handoff_approved": return ["Handoff approved", `${e.stage} → ${e.next ?? "done"}${e.model ? ` · next on ${e.model}` : ""}`];
      case "handoff_sent_back": return ["Sent back", `${e.stage}: ${first(e.notes, 120)}`];
      case "checkpoint_due": return ["Checkpoint due", `${e.context_tokens} tokens · $${(Number(e.session_usd) || 0).toFixed(2)}`];
      case "checkpoint": return [`Checkpoint ${e.n}`, String(e.file ?? "")];
      case "rehydrated": return ["Fresh session", `from ${e.from}`];
      case "agent_removed": return ["Removed", ""];
      case "run_start": return [`Run ${e.run}`, `${e.model || "default model"} · ${e.session} session — ${first(e.prompt)}`];
      case "run_end": return [`Run ${e.run} ${e.state}`, `$${(Number(e.cost_usd) || 0).toFixed(3)}${e.duration_ms ? ` · ${Math.round(e.duration_ms / 1000)}s` : ""}${tokens(e.tokens)}${e.models?.length ? ` · ${e.models.join(", ")}` : ""}`];
      case "herdr_prompt": return ["Prompt (herdr)", first(e.prompt)];
      case "asked": return [ITEM[e.type] ?? e.type, first(e.title)];
      case "answered": return [`${e.status === "answered" ? "Answered" : e.status === "rejected" ? "Declined" : "Dismissed"}`, `${first(e.title, 80)} → ${first(e.answer, 120) || "(no answer)"}`];
      case "decision": return ["Decision", `${first(e.question, 100)} → ${first(e.chosen, 100)}`];
      case "message": return ["Message", `${e.from === "you" ? "you" : e.agent} → ${e.to}: ${first(e.body)}`];
      case "you_prompt": return ["You", `${first(e.text)}${e.queued ? " (queued: it was busy)" : ""}`];
      case "send_now": return ["Send now", e.stopped_run ? "stopped the run to send a queued message" : "sent a queued message"];
      case "unqueued": return ["Took back", "a queued message"];
      case "status": return ["Status", first(e.status)];
      case "note": return ["Note", `${e.key} = ${first(e.value, 120)}`];
      case "report": return ["Report", `${first(e.title, 80)}${e.summary ? ` — ${first(e.summary, 100)}` : ""}`];
      case "wake": return ["Woken", e.reason === "interrupted" ? "after an interrupted run" : `${e.messages ?? 0} messages, ${e.answers ?? 0} answers`];
      case "wake_paused": return ["Wake paused", "too many runs in 10 minutes"];
      case "stopped": return ["Stopped", `by ${e.by}`];
      case "resumed": return ["Resumed", `by ${e.by}${e.text ? ` — ${first(e.text)}` : ""}`];
      case "sprint_joined": return ["In the sprint", `${e.status}`];
      case "sprint_left": return ["Left the sprint", `was ${e.status}`];
      case "jira_status": return ["Jira status", `${e.from} → ${e.to}`];
      case "jira_assignee": return ["Jira assignee", `${e.from || "nobody"} → ${e.to || "nobody"}`];
      case "jira_comment": return ["Jira comment", first(e.latest)];
      case "ticket_set": return ["Took the ticket", e.before ? `${e.before} → ${e.ticket}` : e.ticket];
      case "jira_moved": return ["Moved in Jira", `transition ${e.transition}, when you dispatched it`];
      case "pr_linked": return [`PR #${e.number}`, `${e.repo} · ${first(e.title, 100)}`];
      case "pr_status": return [`PR #${e.number}`, Object.entries(e.changes ?? {}).map(([k, v]: [string, any]) => `${k} ${v[0] || "—"} → ${v[1] || "—"}`).join(" · ")];
      case "pr_merged": return [`PR #${e.number} merged`, first(e.title, 120)];
      case "pr_closed": return [`PR #${e.number} closed`, first(e.title, 120)];
      case "retro_started": return ["Retro started", `${e.agent} · ${(e.tickets ?? []).join(", ")}`];
      case "retro_proposals": return ["Retro proposals", `${e.count} to review in Pipeline`];
      case "proposal_approved": return ["Proposal approved", `${e.proposal_kind}: ${first(e.title, 100)}${e.result ? ` — ${first(e.result, 80)}` : ""}`];
      case "proposal_dismissed": return ["Proposal dismissed", `${e.proposal_kind}: ${first(e.title, 100)}`];
      default: return [e.kind, ""];
    }
  }
  const tone = (k: string) =>
    k === "answered" || k === "decision" || k === "asked" ? "dec"
    : k.startsWith("run") ? "run"
    : k === "you_prompt" || k === "send_now" || k === "stopped" || k === "resumed" ? "you"
    : k.startsWith("jira_") || k.startsWith("pr_") || k.startsWith("sprint_") ? "out"
    : "";

  const DETAIL_SKIP = new Set(["ts", "at", "ticket", "kind", "agent", "group", "role"]);
  const details = (e: RecordEvent) =>
    Object.entries(e).filter(([k, v]) => !DETAIL_SKIP.has(k) && v !== null && v !== "" && !(Array.isArray(v) && !v.length));
  const show = (v: unknown) => (typeof v === "string" ? v : JSON.stringify(v, null, 2));

  const day = (ts: number) => new Date(ts * 1000).toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
  const time = (ts: number) => new Date(ts * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });

  const here = $derived(records.filter((r) => r.ticket === ticket));
</script>

<Modal title={`Timeline · ${ticket}`} wide onclose={() => (app.timeline = "")}>
  <div class="bar row">
    <select bind:value={app.timeline} aria-label="Ticket">
      {#if !records.some((r) => r.ticket === ticket)}<option value={ticket}>{ticket}</option>{/if}
      {#each [...new Set(records.map((r) => r.ticket))] as t}<option value={t}>{t}</option>{/each}
    </select>
    <div class="filters grow">
      {#each FILTERS as [key, name]}
        <button class="chip" class:on={filter === key} onclick={() => (filter = key)}>{name}</button>
      {/each}
    </div>
    <button class="ghost" disabled={!events.length} title="Look back at this ticket and get proposals with evidence"
      onclick={() => api.startRetro([ticket]).then((a) => { toast(`Retro started: ${a.name}. Its proposals show in Pipeline.`); }).catch((e) => toast(String(e)))}>Start retro</button>
    {#each here as r}
      <button class="ghost" title={r.path} onclick={() => api.openPath(r.path).catch((e) => toast(String(e)))}>Show folder{here.length > 1 ? ` (${r.folder || "plans"})` : ""}</button>
    {/each}
  </div>

  {#if events.length}
    <div class="totals muted">
      <span>{totals.agents} agent{totals.agents === 1 ? "" : "s"}</span>
      <span>{totals.runs} run{totals.runs === 1 ? "" : "s"} · ${totals.cost.toFixed(2)}</span>
      <span>{totals.asked} asked · {totals.declined} declined or dismissed</span>
      <span>{totals.decisions} decision{totals.decisions === 1 ? "" : "s"}</span>
      <span>{totals.wakes} wake{totals.wakes === 1 ? "" : "s"}</span>
    </div>
  {/if}

  <div class="list">
    {#each shown as e, i (i)}
      {@const [what, text] = label(e)}
      {#if i === 0 || day(shown[i - 1].ts) !== day(e.ts)}<div class="day">{day(e.ts)}</div>{/if}
      <button class="ev {tone(e.kind)}" onclick={() => (open[i] = !open[i])} aria-expanded={!!open[i]}>
        <span class="t muted" title={e.at}>{time(e.ts)}</span>
        <span class="k">{what}</span>
        <span class="a muted">{e.agent ?? (e.source === "watcher" ? "watcher" : e.group ? `@${e.group}` : "")}</span>
        <span class="x">{text}</span>
      </button>
      {#if open[i]}
        <dl class="more">
          {#each details(e) as [k, v]}<dt>{k}</dt><dd><pre>{show(v)}</pre></dd>{/each}
        </dl>
      {/if}
    {:else}
      <div class="empty">
        {#if !loaded}Loading…{:else if events.length}Nothing of this kind yet.{:else}No record for {ticket} yet. It starts with the next thing an agent on this ticket does.{/if}
      </div>
    {/each}
  </div>
</Modal>

<style>
  .bar { gap: 8px; align-items: center; flex-wrap: wrap; }
  .bar select { width: 140px; }
  .filters { display: flex; gap: 4px; flex-wrap: wrap; }
  .chip { font-size: 12px; padding: 2px 9px; border-radius: 999px; background: transparent; }
  .chip.on { border-color: var(--accent); color: var(--accent); }
  .totals { display: flex; flex-wrap: wrap; gap: 14px; font-size: 12px; margin: 10px 0 4px; }
  .list { margin-top: 6px; max-height: calc(100vh - 230px); overflow: auto; border-top: 1px solid var(--border); }
  .day { font-size: 11px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); padding: 10px 2px 4px; }
  .ev { display: grid; grid-template-columns: 52px 140px 150px 1fr; gap: 10px; width: 100%; text-align: left; background: transparent;
    border: none; border-bottom: 1px solid var(--border); border-radius: 0; padding: 6px 2px; font-size: 13px; align-items: baseline; }
  .ev:hover { background: var(--panel-2); }
  .ev .k { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ev .a { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 12px; }
  .ev .x { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ev.dec .k { color: var(--yellow); }
  .ev.run .k { color: var(--cyan); }
  .ev.you .k { color: var(--purple); }
  .ev.out .k { color: var(--green); }
  .t { font-family: var(--mono); font-size: 11px; }
  .more { display: grid; grid-template-columns: 120px 1fr; gap: 4px 10px; margin: 4px 0 8px 62px; font-size: 12px; }
  .more dt { color: var(--muted); }
  .more dd { margin: 0; }
  .more pre { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; max-height: 260px; overflow: auto; font-family: var(--mono); }
  .empty { padding: 24px 4px; color: var(--muted); }
</style>
