<script lang="ts">
  import { api, ago, type Proposal, type Retro, type TicketRecord } from "#lib/api.ts";
  import { app, act, select, toast } from "#lib/state.svelte.ts";
  import Modal from "./Modal.svelte";

  // Retros you started, with their proposals: approve one (which applies
  // it) or dismiss it, or approve all that are left.
  let retros = $state<Retro[]>([]);
  let starting = $state(false);
  let records = $state<TicketRecord[]>([]);
  let picked = $state<Record<string, boolean>>({});
  let note = $state("");
  let showReviewed = $state(false);
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
    records = await api.records().catch(() => []);
    picked = {};
    showReviewed = false;
    starting = true;
  }

  async function start() {
    const tickets = Object.keys(picked).filter((k) => picked[k]);
    const a = await act(() => api.startRetro(tickets, note), `Retro started on ${tickets.join(", ")}`);
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
  // One line per ticket (it may have records in several groups' folders):
  // when its last retro ran, and whether it's had work since.
  type Pick = { ticket: string; lastRetro: number; fresh: boolean };
  const picks = (rs: TicketRecord[]): Pick[] => {
    const by = new Map<string, { lastRetro: number; lastWork: number }>();
    for (const r of rs) {
      const t = by.get(r.ticket) ?? { lastRetro: 0, lastWork: 0 };
      by.set(r.ticket, { lastRetro: Math.max(t.lastRetro, r.last_retro), lastWork: Math.max(t.lastWork, r.last_work) });
    }
    return [...by].map(([ticket, t]) => ({ ticket, lastRetro: t.lastRetro, fresh: !t.lastRetro || t.lastWork > t.lastRetro }));
  };
  // A retro is finished once its agent has stopped and you've decided every
  // proposal; finished ones fold away under a toggle.
  const isActive = (r: Retro) => agentState(r.agent) === "working" || r.proposals.some((p) => p.status === "pending");
  const active = $derived(retros.filter(isActive));
  const finished = $derived(retros.filter((r) => !isActive(r)));
  const summary = (r: Retro) => {
    const n = (st: string) => r.proposals.filter((p) => p.status === st).length;
    return r.proposals.length ? `${n("approved")} approved · ${n("dismissed")} dismissed` : "no proposals";
  };
  const all = $derived(picks(records));
  const fresh = $derived(all.filter((p) => p.fresh));
  const reviewed = $derived(all.filter((p) => !p.fresh));
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
    <div class="picks">
      {#snippet pick(p: Pick)}
        <label class="row pick">
          <input type="checkbox" bind:checked={picked[p.ticket]} /> {p.ticket}
          {#if p.lastRetro}<span class="muted small">retro {ago(p.lastRetro)} ago · {p.fresh ? "new work since" : "nothing new since"}</span>{/if}
        </label>
      {/snippet}
      {#each fresh as p (p.ticket)}
        {@render pick(p)}
      {:else}
        <div class="muted">{all.length ? "Every ticket has had a retro since its last work." : "No ticket has a record yet."}</div>
      {/each}
      {#if reviewed.length}
        <button class="ghost small" onclick={() => (showReviewed = !showReviewed)}>{showReviewed ? "▾" : "▸"} {reviewed.length} with nothing new since their retro</button>
        {#if showReviewed}{#each reviewed as p (p.ticket)}{@render pick(p)}{/each}{/if}
      {/if}
    </div>
    <input placeholder="Anything to look at in particular (optional)" bind:value={note} />
    {#snippet footer()}
      <button onclick={() => (starting = false)}>Cancel</button>
      <button class="primary" disabled={!Object.values(picked).some(Boolean)} onclick={start}>Start retro</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .retros { border-bottom: 1px solid var(--border); padding: 8px 0 12px; margin-bottom: 4px; }
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
  .pick { gap: 6px; font-size: 13px; }
  .pick input { width: auto; }
</style>
