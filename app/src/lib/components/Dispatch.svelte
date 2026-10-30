<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Defaults } from "$lib/api";
  import { app, act, select } from "$lib/state.svelte";
  import Modal from "./Modal.svelte";
  import RunDefaults from "./RunDefaults.svelte";
  import { nameInput, NAME_FIELD } from "$lib/slug";

  const ticket = app.dispatching!;
  // With the ticket workflow, a ticket gets its own group and lead, which runs its stages.
  let mode = $state<"solo" | "group" | "existing">(app.defaults?.workflow ? "group" : app.snap.groups.length ? "existing" : "solo");
  let group = $state(app.snap.groups[0]?.name ?? "");
  let target = $state("");
  const members = $derived(app.snap.agents.filter((a) => a.group_name === group));
  const lead = $derived(members.find((a) => a.role === "orchestrator"));
  // Default to the group's lead; a group without one needs an agent picked.
  $effect(() => {
    group;
    target = "";
  });
  const recipient = $derived(target || lead?.name || "");
  let name = $state(ticket.key.toLowerCase());
  let defaults = $state<Defaults | null>(null);
  let cwd = $state("");
  let backend = $state("");
  let harness = $state("");
  let notes = $state("");
  let busy = $state(false);

  onMount(async () => {
    defaults = await api.defaults();
    if (!cwd) cwd = defaults.cwd;
  });

  async function go() {
    busy = true;
    const a = await act(
      () => api.dispatch(ticket, mode, name, cwd, backend, harness, notes, group, target),
      mode === "existing" ? `${ticket.key} sent to ${recipient}` : `${ticket.key} handed off`,
    );
    busy = false;
    if (a) { app.dispatching = null; select(a.name); }
  }
</script>

<Modal title={`Hand off ${ticket.key}`} onclose={() => (app.dispatching = null)} dirty={!!notes.trim()}>
  <p class="muted">{ticket.summary}</p>
  <div class="modes">
    <button class:on={mode === "solo"} onclick={() => (mode = "solo")}>
      <strong>Single agent</strong><span class="muted">One agent works the ticket and reports back.</span>
    </button>
    <button class:on={mode === "group"} onclick={() => (mode = "group")}>
      <strong>New group + orchestrator</strong><span class="muted">A lead plans the work, spawns workers that coordinate, and brings you decisions.</span>
    </button>
    <button class:on={mode === "existing"} onclick={() => (mode = "existing")} disabled={!app.snap.groups.length}>
      <strong>Existing group</strong><span class="muted">{app.snap.groups.length ? "Give it to a group you already have; its lead brings in the others." : "You have no groups yet."}</span>
    </button>
  </div>
  {#if mode === "existing"}
    <label for="d-group">Group</label>
    <select id="d-group" bind:value={group}>
      {#each app.snap.groups as g}<option value={g.name}>{g.name}</option>{/each}
    </select>
    <label for="d-target">Send to</label>
    <select id="d-target" bind:value={target}>
      {#if lead}<option value="">{lead.name} (lead)</option>{:else}<option value="" disabled>Choose an agent…</option>{/if}
      {#each members.filter((a) => a.role !== "orchestrator") as a}<option value={a.name}>{a.name}</option>{/each}
    </select>
    {#if recipient}
      {@const r = members.find((a) => a.name === recipient)}
      <p class="muted small">
        {recipient} gets the ticket{r?.state === "working" ? " as a message it reads when it finishes what it's doing" : " as its next prompt"}{r?.role === "orchestrator" ? ", and brings in the group's agents" : ""}.
      </p>
    {/if}
  {:else}
  <label for="d-name">{mode === "group" ? "Group name" : "Agent name"}</label>
  <input id="d-name" value={name} oninput={(e) => (name = nameInput(e))} {...NAME_FIELD} />
  <label for="d-cwd">Working directory (repo)</label>
  <input id="d-cwd" bind:value={cwd} />
  <RunDefaults {defaults} bind:backend bind:harness id="d-backend" />
  {/if}
  <label for="d-notes">Notes for the agent</label>
  <textarea id="d-notes" rows="4" bind:value={notes} placeholder="Constraints, approach, where to look, what not to touch…"></textarea>
  {#snippet footer()}
    <button onclick={() => (app.dispatching = null)}>Cancel</button>
    <button class="primary" disabled={busy || (mode === "existing" ? !recipient : !name || !backend)} onclick={go}>{busy ? "Starting…" : "Start"}</button>
  {/snippet}
</Modal>

<style>
  .modes { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 8px; }
  .small { font-size: 12px; margin: 6px 0 0; }
  .modes button { display: flex; flex-direction: column; gap: 4px; text-align: left; padding: 10px; }
  .modes button.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, var(--panel-2)); }
  .modes span { font-size: 12px; }
</style>
