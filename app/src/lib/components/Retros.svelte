<script lang="ts">
  import { api, ago, type Proposal, type Retro, type RetroTicket } from "#lib/api.ts";
  import { app, act, select, toast } from "#lib/state.svelte.ts";
  import Modal from "./Modal.svelte";

  // Retros you started, with their proposals: approve one (which applies
  // it) or dismiss it, or approve all that are left.
  let retros = $state<Retro[]>([]);
  let starting = $state(false);
  let tickets = $state<RetroTicket[]>([]);
  let list = $state<RetroTicket["list"]>("not_covered");
  let picked = $state<Record<string, boolean>>({});
  let note = $state("");
  let showFinished = $state(false);
  let open = $state<Record<string, boolean>>({});

  async function load() {
    try { retros = await api.retros(); } catch {}
  }
  $effect(() => {
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });

  async function startDialog() {
    await loadTickets();
    // New work is picked: the next retro is for it.
    picked = Object.fromEntries(tickets.filter((t) => isNew(t)).map((t) => [t.ticket, true]));
    list = LISTS.find(([id]) => inList(id).length)?.[0] ?? "not_covered";
    starting = true;
  }

  async function start() {
    const a = await act(() => api.startRetro(chosen, note), `Retro started on ${chosen.join(", ")}`);
    if (a) { starting = false; note = ""; load(); }
  }

  async function decide(r: Retro, p: Proposal, approve: boolean) {
    const res = await act(() => api.decideProposal(r.dir, p.id, approve));
    if (res !== undefined && approve) toast(res || "Approved");
    load();
  }

  async function approveAll(r: Retro) {
    for (const p of r.proposals.filter((p) => p.status === "pending")) {
      const ok = await act(() => api.decideProposal(r.dir, p.id, true));
      if (ok === undefined) break;
    }
    load();
  }

  const agentState = (name: string) => app.snap.agents.find((a) => a.name === name)?.state ?? "finished";
  const change = (p: Proposal) =>
    p.change?.config ? `${p.change.config.key} = ${JSON.stringify(p.change.config.value)}`
    : p.change?.skill ? `skill ${p.change.skill.name}` : "a change you make yourself";
  const LISTS: [RetroTicket["list"], string, string][] = [
    ["not_covered", "Not covered", "No retro has read these."],
    ["new_work", "New work", "Agents, PRs or reviews moved since the last retro, which is where the next one starts. Jira status and cleanup don't count."],
    ["covered", "Covered", "A retro has read everything that matters here. Picked again, it reads all of it."],
    ["hidden", "Hidden", "Out of the lists until you show them, or until there's new work on them. Their records are kept."],
  ];
  const inList = (id: string) => tickets.filter((t) => t.list === id);
  const isNew = (t: RetroTicket) => t.list === "not_covered" || t.list === "new_work";
  const chosen = $derived(tickets.filter((t) => picked[t.ticket] && t.list !== "hidden").map((t) => t.ticket));
  async function loadTickets() {
    try { tickets = await api.retroTickets(); } catch (e) { toast(String(e)); }
  }
  async function hide(t: RetroTicket, hidden: boolean) {
    await act(() => api.setRetroHidden(t.ticket, hidden));
    if (hidden) picked[t.ticket] = false;
    await loadTickets();
  }
  const did = (t: RetroTicket) => {
    const what = [t.runs && `${t.runs} run${t.runs === 1 ? "" : "s"}`, t.merged && `${t.merged} PR${t.merged === 1 ? "" : "s"} merged`, t.cost && `$${t.cost.toFixed(2)}`].filter(Boolean).join(", ");
    return `${what || "no runs"}${t.retros.length ? " since" : ""}`;
  };
  const day = (ts: number) => new Date(ts * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const covered = (t: RetroTicket) => (t.retros.length ? `Retro ${t.retros.map(day).join(", ")}` : "Never in a retro");
  // A retro is finished once its agent has stopped and you've decided every
  // proposal; finished ones fold away under a toggle.
  const isActive = (r: Retro) => agentState(r.agent) === "working" || r.proposals.some((p) => p.status === "pending");
  const active = $derived(retros.filter(isActive));
  const finished = $derived(retros.filter((r) => !isActive(r)));
  const summary = (r: Retro) => {
    const n = (st: string) => r.proposals.filter((p) => p.status === st).length;
    return r.proposals.length ? `${n("approved")} approved · ${n("dismissed")} dismissed` : "no proposals";
  };
</script>

<section class="retros">
  <div class="row head">
    <h3 class="grow">Retros</h3>
    <button onclick={startDialog} title="Look back at tickets and get proposals to improve the process, skills and models">Retro…</button>
  </div>
  {#snippet card(r: Retro, finished: boolean)}
    {@const pending = r.proposals.filter((p) => p.status === "pending")}
    <div class="retro">
      <div class="row">
        <strong class="grow">{r.tickets.join(", ")}</strong>
        <span class="muted small">{ago(r.started)} ago · <button class="link" onclick={() => select(r.agent)}>{r.agent}</button> {agentState(r.agent)}</span>
        {#if finished}<button class="ghost small" onclick={() => (open[r.dir] = !open[r.dir])}>{open[r.dir] ? "▾" : "▸"} {summary(r)}</button>{/if}
        {#if pending.length > 1}<button onclick={() => approveAll(r)}>Approve all ({pending.length})</button>{/if}
      </div>
      {#if !finished || open[r.dir]}
      {#if !r.proposals.length}<div class="muted small">No proposals yet{agentState(r.agent) === "working" ? "; it's still looking" : ""}.</div>{/if}
      {#each r.proposals as p (p.id)}
        <div class="prop" class:done={p.status !== "pending"}>
          <div class="row">
            <span class="pill">{p.kind.replace("_", " ")}</span>
            <strong class="grow">{p.title}</strong>
            {#if p.status === "pending"}
              <button class="primary" onclick={() => decide(r, p, true)}>Approve</button>
              <button class="ghost" onclick={() => decide(r, p, false)}>Dismiss</button>
            {:else}<span class="muted small">{p.status}</span>{/if}
          </div>
          {#if p.why}<div class="why">{p.why}</div>{/if}
          <div class="muted small">Changes: <code>{change(p)}</code>{#if p.result} · {p.result}{/if}</div>
          <button class="ghost small" onclick={() => (open[r.dir + p.id] = !open[r.dir + p.id])}>{open[r.dir + p.id] ? "▾" : "▸"} Evidence ({p.evidence.length}){p.change?.skill ? " and the skill" : ""}</button>
          {#if open[r.dir + p.id]}
            <ul class="ev">{#each p.evidence as e}<li>{e}</li>{/each}</ul>
            {#if p.change?.skill}<pre>{p.change.skill.content}</pre>{/if}
          {/if}
        </div>
      {/each}
      {/if}
    </div>
  {/snippet}
  {#each active as r (r.dir)}
    {@render card(r, false)}
  {:else}
    {#if !finished.length}<div class="muted small">No retros yet. Start one here or from a ticket's timeline when you want to look back.</div>{/if}
  {/each}
  {#if finished.length}
    <button class="ghost small" onclick={() => (showFinished = !showFinished)}>{showFinished ? "▾" : "▸"} {finished.length} finished {finished.length === 1 ? "retro" : "retros"}</button>
    {#if showFinished}{#each finished as r (r.dir)}{@render card(r, true)}{/each}{/if}
  {/if}
</section>

{#if starting}
  <Modal title="Start a retro" onclose={() => (starting = false)}>
    <p class="muted small">A retro agent reads these tickets' records and the numbers drovr works out from them (cost by stage and model, rework, dismissals, misses), then proposes changes with evidence. Nothing changes until you approve it.</p>
    <div class="row lists">
      {#each LISTS as [id, name]}<button class="ghost" class:on={list === id} aria-pressed={list === id} onclick={() => (list = id)}>{name} {inList(id).length}</button>{/each}
    </div>
    <p class="muted small">{LISTS.find(([id]) => id === list)?.[2]}</p>
    <div class="picks">
      {#each inList(list) as t (t.ticket)}
        <div class="row pick">
          {#if list === "hidden"}<span class="grow"><strong>{t.ticket}</strong><span class="muted small">{did(t)} · {covered(t)}</span></span>
          {:else}<label class="row grow"><input type="checkbox" bind:checked={picked[t.ticket]} /><span><strong>{t.ticket}</strong><span class="muted small">{did(t)} · {covered(t)}</span></span></label>{/if}
          <button class="ghost small" onclick={() => hide(t, list !== "hidden")}>{list === "hidden" ? "Show" : "Hide"}</button>
        </div>
      {:else}
        <div class="muted">{tickets.length ? "None." : "No ticket has a record yet."}</div>
      {/each}
    </div>
    <input placeholder="Anything to look at in particular (optional)" bind:value={note} />
    {#snippet footer()}
      <button onclick={() => (starting = false)}>Cancel</button>
      <button class="primary" disabled={!chosen.length} onclick={start}>Start retro on {chosen.length} {chosen.length === 1 ? "ticket" : "tickets"}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .head h3 { margin: 4px 0; font-size: 14px; }
  .retro { border: 1px solid var(--border); border-radius: 8px; padding: 8px 10px; margin: 8px 0; background: var(--panel); }
  .prop { border-top: 1px solid var(--border); padding: 6px 0; }
  .prop.done { opacity: .6; }
  .why { font-size: 13px; margin: 2px 0; }
  .small { font-size: 11px; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font-size: 11px; }
  .ev { margin: 4px 0 4px 18px; font-size: 12px; }
  pre { font-family: var(--mono); font-size: 12px; white-space: pre-wrap; max-height: 260px; overflow: auto; background: var(--bg); padding: 6px; border-radius: 4px; }
  .picks { max-height: 260px; overflow: auto; margin: 8px 0; }
  .lists { flex-wrap: wrap; gap: 6px; margin: 8px 0 4px; }
  .lists .on { background: var(--panel-2); border-color: var(--accent); }
  .pick { gap: 8px; padding: 6px 0; border-top: 1px solid var(--border); font-size: 13px; }
  .pick span span { display: block; }
  .pick label { margin: 0; font-size: inherit; font-weight: inherit; letter-spacing: normal; text-transform: none; color: inherit; }
  .pick input { width: auto; }
</style>
