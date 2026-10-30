<script lang="ts">
  import { api, ago, type Proposal, type Retro, type TicketRecord } from "$lib/api";
  import { app, act, select, toast } from "$lib/state.svelte";
  import Modal from "./Modal.svelte";

  // Retros you started, with their proposals: approve one (which applies
  // it) or dismiss it, or approve all that are left.
  let retros = $state<Retro[]>([]);
  let starting = $state(false);
  let records = $state<TicketRecord[]>([]);
  let picked = $state<Record<string, boolean>>({});
  let note = $state("");
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
  const uniq = (rs: TicketRecord[]) => [...new Set(rs.map((r) => r.ticket))];
</script>

<section class="retros">
  <div class="row head">
    <h3 class="grow">Retros</h3>
    <button onclick={startDialog} title="Look back at tickets and get proposals to improve the process, skills and models">Retro…</button>
  </div>
  {#each retros as r (r.dir)}
    {@const pending = r.proposals.filter((p) => p.status === "pending")}
    <div class="retro">
      <div class="row">
        <strong class="grow">{r.tickets.join(", ")}</strong>
        <span class="muted small">{ago(r.started)} ago · <button class="link" onclick={() => select(r.agent)}>{r.agent}</button> {agentState(r.agent)}</span>
        {#if pending.length > 1}<button onclick={() => approveAll(r)}>Approve all ({pending.length})</button>{/if}
      </div>
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
    </div>
  {:else}
    <div class="muted small">No retros yet. Start one here or from a ticket's timeline when you want to look back.</div>
  {/each}
</section>

{#if starting}
  <Modal title="Start a retro" onclose={() => (starting = false)}>
    <p class="muted small">A retro agent reads these tickets' records and the numbers drovr works out from them (cost by stage and model, rework, dismissals, misses), then proposes changes with evidence. Nothing changes until you approve it.</p>
    <div class="picks">
      {#each uniq(records) as t}
        <label class="row pick"><input type="checkbox" bind:checked={picked[t]} /> {t}</label>
      {:else}
        <div class="muted">No ticket has a record yet.</div>
      {/each}
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
