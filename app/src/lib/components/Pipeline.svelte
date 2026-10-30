<script lang="ts">
  import { api, ago, ticketsIn, type Agent, type SprintTicket, type WatchState, type WatchedPr } from "$lib/api";
  import { app, toast, select } from "$lib/state.svelte";
  import Retros from "./Retros.svelte";

  // Your sprint as the watcher sees it (Jira through your CLI, PRs through
  // gh), next to the agents working each ticket. Part of the ticket workflow.
  let ws = $state<WatchState | null>(null);
  let busy = $state(false);
  let showDone = $state(false);

  async function load() {
    try { ws = await api.watchState(); } catch (e) { toast(String(e)); }
  }
  $effect(() => {
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });

  async function refreshNow() {
    busy = true;
    try { ws = await api.watchNow(); } catch (e) { toast(String(e)); }
    busy = false;
  }

  // Agents on a ticket: its own Ticket field, or its group's.
  const groupTicket = (a: Agent) => app.snap.groups.find((g) => g.name === a.group_name)?.ticket ?? "";
  const agentsFor = (key: string): Agent[] =>
    app.snap.agents.filter((a) => ticketsIn(a.ticket).includes(key) || (!ticketsIn(a.ticket).length && ticketsIn(groupTicket(a)).includes(key)));
  const prsFor = (key: string): WatchedPr[] => Object.values(ws?.prs ?? ({} as Record<string, WatchedPr>)).filter((p) => p.tickets.includes(key));
  const waiting = (agents: Agent[]) => app.snap.inbox.filter((i) => agents.some((a) => a.name === i.agent) && i.kind !== "done").length;
  const cost = (agents: Agent[]) => agents.reduce((s, a) => s + a.cost_usd, 0);

  const tickets = $derived(Object.values(ws?.tickets ?? ({} as Record<string, SprintTicket>)).sort((a, b) => b.updated.localeCompare(a.updated)));
  const open = $derived(tickets.filter((t) => t.category !== "done"));
  const done = $derived(tickets.filter((t) => t.category === "done"));
  // Tickets agents are on that aren't in the sprint the watcher follows.
  const others = $derived.by(() => {
    const keys = new Set(tickets.map((t) => t.key));
    const all = new Set(app.snap.agents.flatMap((a) => (ticketsIn(a.ticket).length ? ticketsIn(a.ticket) : ticketsIn(groupTicket(a)))));
    return [...all].filter((k) => !keys.has(k)).sort();
  });

  const tone = (t: SprintTicket) => (t.category === "done" ? "done" : t.category === "indeterminate" ? "doing" : "todo");
  const ci = (p: WatchedPr) => ({ success: "✓", failure: "✗", pending: "●", none: "" })[p.ci] ?? "";
  const review = (p: WatchedPr) => ({ APPROVED: "approved", CHANGES_REQUESTED: "changes requested", REVIEW_REQUIRED: "needs review" })[p.review] ?? "";
  const since = (at: number) => (at ? `${ago(at)} ago` : "not yet");

  async function dispatch(key: string) {
    try { app.dispatching = await api.jiraGet(key); } catch (e) { toast(String(e)); }
  }
</script>

{#snippet row(key: string, t: SprintTicket | null)}
  {@const agents = agentsFor(key)}
  {@const prs = prsFor(key)}
  {@const w = waiting(agents)}
  <tr>
    <td class="key">
      {#if t?.url}<button class="link" onclick={() => api.openUrl(t.url)} title="Open in Jira">{key}</button>{:else}<strong>{key}</strong>{/if}
      <div class="sum">{t?.summary ?? ""}</div>
      {#if t?.last_comment}<div class="muted small" title={t.last_comment}>💬 {t.last_comment}</div>{/if}
    </td>
    <td>{#if t}<span class="pill j-{tone(t)}">{t.status}</span>{:else}<span class="muted small">not in the sprint</span>{/if}</td>
    <td class="agents">
      {#each agents as a}
        <button class="chip" onclick={() => select(a.name)} title={a.status || a.task.split("\n")[0]}>{a.role === "orchestrator" ? "★ " : ""}{a.name}{a.stage ? ` · ${a.stage}` : ""} <span class="pill s-{a.state}">{a.state}</span></button>
      {:else}
        <span class="muted small">none</span>
      {/each}
    </td>
    <td class="prs">
      {#each prs as p}
        <button class="chip pr" onclick={() => api.openUrl(p.url)} title={`${p.repo}#${p.number} ${p.title}${p.failing.length ? `\nFailing: ${p.failing.join(", ")}` : ""}`}>
          #{p.number}
          {#if p.state === "MERGED"}<span class="ok">merged</span>
          {:else if p.state === "CLOSED"}<span class="muted">closed</span>
          {:else}
            {#if ci(p)}<span class="ci c-{p.ci}">{ci(p)}</span>{/if}
            {#if p.draft}<span class="muted">draft</span>{/if}
            {#if review(p)}<span class:warn={p.review === "CHANGES_REQUESTED"} class:ok={p.review === "APPROVED"}>{review(p)}</span>{/if}
            {#if p.mergeable === "CONFLICTING"}<span class="warn">conflict</span>{/if}
          {/if}
        </button>
      {/each}
    </td>
    <td>{#if w}<button class="chip warn" onclick={() => (app.view = "inbox")}>⚑ {w}</button>{/if}</td>
    <td class="num">{agents.length ? `$${cost(agents).toFixed(2)}` : ""}</td>
    <td class="actions">
      {#if agents.length}<button class="ghost" onclick={() => (app.timeline = key)}>Timeline</button>
      {:else if t}<button onclick={() => dispatch(key)}>Dispatch…</button>{/if}
    </td>
  </tr>
{/snippet}

<div class="wrap">
  <header class="row">
    <h2 class="grow">Your sprint <span class="muted small">{open.length} open · {done.length} done</span></h2>
    <span class="muted small" class:err={ws?.jira.error} title={ws?.jira.error || ""}>Jira {ws?.jira.error ? "⚠" : ""} read {since(ws?.jira.at ?? 0)}</span>
    <span class="muted small" class:err={ws?.github.error} title={ws?.github.error || ""}>GitHub {ws?.github.error ? "⚠" : ""} read {since(ws?.github.at ?? 0)}</span>
    <button onclick={refreshNow} disabled={busy}>{busy ? "Reading…" : "Refresh now"}</button>
  </header>
  {#if ws?.jira.error}<div class="banner err">Jira: {ws.jira.error}</div>{/if}
  {#if ws?.github.error}<div class="banner err">GitHub: {ws.github.error}</div>{/if}

  <div class="scroll grow">
    <Retros />
    {#if !ws || (!ws.jira.at && !tickets.length)}
      <div class="empty">The watcher hasn't read Jira yet. It reads the tickets matching <code>watch.jql</code> through your Jira command (<code>jira.provider = "command"</code>, e.g. twg), and their PRs from <code>watch.github_repos</code> with gh. <button onclick={refreshNow} disabled={busy}>Read now</button></div>
    {:else}
      <table>
        <thead><tr><th>Ticket</th><th>Jira</th><th>Agents</th><th>Pull requests</th><th>Waiting</th><th>Cost</th><th></th></tr></thead>
        <tbody>
          {#each open as t (t.key)}{@render row(t.key, t)}{/each}
          {#each others as key (key)}{@render row(key, null)}{/each}
        </tbody>
        {#if done.length}
          <tbody>
            <tr><td colspan="7"><button class="ghost" onclick={() => (showDone = !showDone)}>{showDone ? "▾" : "▸"} Done ({done.length})</button></td></tr>
            {#if showDone}{#each done as t (t.key)}{@render row(t.key, t)}{/each}{/if}
          </tbody>
        {/if}
      </table>
    {/if}
  </div>
</div>

<style>
  .wrap { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  header { padding: 10px 16px; border-bottom: 1px solid var(--border); gap: 12px; align-items: center; }
  h2 { margin: 0; font-size: 16px; }
  .small { font-size: 11px; }
  .err { color: var(--red); }
  .banner { padding: 6px 16px; font-size: 12px; border-bottom: 1px solid var(--border); white-space: pre-wrap; }
  .scroll { overflow: auto; padding: 0 16px 16px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); padding: 10px 6px 6px; position: sticky; top: 0; background: var(--bg); }
  td { padding: 8px 6px; border-top: 1px solid var(--border); vertical-align: top; }
  .key { max-width: 340px; }
  .key .sum { margin-top: 2px; }
  .key .muted { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 340px; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font-weight: 600; }
  .chip { font-size: 12px; padding: 2px 8px; border-radius: 999px; background: transparent; margin: 0 4px 4px 0; display: inline-flex; gap: 5px; align-items: center; }
  .chip .pill { font-size: 10px; padding: 0 5px; }
  .pr span { font-size: 11px; }
  .ci.c-success, .ok { color: var(--green); }
  .ci.c-failure, .warn { color: var(--yellow); }
  .ci.c-failure { color: var(--red); }
  .ci.c-pending { color: var(--cyan); }
  .j-todo { color: var(--muted); }
  .j-doing { color: var(--cyan); border-color: var(--cyan); }
  .j-done { color: var(--green); border-color: var(--green); }
  .num { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .actions { white-space: nowrap; text-align: right; }
  .empty { padding: 24px 0; color: var(--muted); max-width: 70ch; }
</style>
