<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Defaults, type PlansFolder } from "$lib/api";
  import { app, act, select, toast } from "$lib/state.svelte";
  import { slug, nameInput, NAME_FIELD } from "$lib/slug";
  import Modal from "./Modal.svelte";
  import RunDefaults from "./RunDefaults.svelte";

  let defaults = $state<Defaults | null>(null);
  let folders = $state<PlansFolder[]>([]);
  let name = $state("");
  let nameTouched = $state(false);
  let plansDir = $state(""); // "" = a new folder named after the group
  let cwd = $state("");
  let ticket = $state("");
  let lead = $state(false);
  let goal = $state("");
  let backend = $state("");
  let harness = $state("");
  let busy = $state(false);

  const existing = $derived(folders.filter((f) => f.name));

  onMount(async () => {
    try {
      [defaults, folders] = await Promise.all([api.defaults(), api.plansFolders()]);
      cwd = defaults.cwd;
    } catch (e) {
      toast(String(e));
    }
  });

  // Picking a folder names the group after it, until you type a name.
  $effect(() => {
    if (plansDir && !nameTouched) name = slug(plansDir);
  });

  function close() {
    app.newGroupOpen = false;
  }

  async function go() {
    busy = true;
    const spec = { name, goal, ticket, cwd, backend, harness, plans_dir: plansDir, lead };
    const created = await act(async () => {
      const a = await api.createGroup(spec);
      return { a };
    }, lead ? `Group ${name} started` : `Group ${name} created. Add its agents next.`);
    busy = false;
    if (!created) return;
    close();
    if (created.a) {
      select(created.a.name);
    } else {
      // No lead: go straight to adding the group's first agent.
      app.newAgentGroup = name;
      app.newAgentOpen = true;
    }
  }
</script>

<Modal title="New group" onclose={close} dirty={!!goal.trim() || !!ticket.trim()}>
  <p class="muted">Agents in a group coordinate with each other and share a plans folder for reports and prompts.</p>

  <label for="g-plans">Plans folder <span class="where">in {defaults?.plans_dir ?? "~/.claude/plans"}</span></label>
  <select id="g-plans" bind:value={plansDir}>
    <option value="">New folder named after the group</option>
    {#each existing as f}<option value={f.name}>{f.name}{f.prompts.length ? ` · ${f.prompts.length} prompt file${f.prompts.length > 1 ? "s" : ""}` : ""}</option>{/each}
  </select>

  <label for="g-name">Group name</label>
  <input id="g-name" value={name} oninput={(e) => { nameTouched = true; name = nameInput(e); }} {...NAME_FIELD} placeholder="pay-api" />

  <label for="g-cwd">Working directory <span class="where">default for the group's agents</span></label>
  <input id="g-cwd" bind:value={cwd} />

  <label for="g-ticket">Jira ticket (optional)</label>
  <input id="g-ticket" bind:value={ticket} placeholder="SRE-123" />

  <label class="check"><input type="checkbox" bind:checked={lead} /> Start an orchestrator (<code>{name || "name"}-lead</code>)</label>
  <p class="muted small">Without one, the agents you add coordinate with each other directly. You can add a lead later from the group's <b>+ lead</b> button.</p>

  <label for="g-goal">Goal{lead ? "" : " (optional)"} <span class="where">{lead ? "the orchestrator's task" : "shown to the group's agents"}</span></label>
  <textarea id="g-goal" rows="4" bind:value={goal} placeholder="What the group is working toward"></textarea>

  {#if lead}<RunDefaults {defaults} bind:backend bind:harness id="g-backend" />{/if}

  {#snippet footer()}
    <button onclick={close}>Cancel</button>
    <button class="primary" disabled={busy || !name || (lead && (!goal.trim() || !backend))} onclick={go}>
      {busy ? "Creating…" : lead ? "Create and start lead" : "Create, then add agents"}
    </button>
  {/snippet}
</Modal>

<style>
  .where { text-transform: none; letter-spacing: 0; opacity: 0.8; margin-left: 4px; }
  .check { display: flex; align-items: center; gap: 8px; text-transform: none; letter-spacing: 0; font-size: 13px; color: var(--text); margin-top: 14px; }
  .check input { width: auto; }
  .small { font-size: 12px; margin: 4px 0 0; }
</style>
