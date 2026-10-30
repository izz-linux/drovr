<script lang="ts">
  import { onMount } from "svelte";
  import { api, ago, type ClaudeSession, type Defaults, type PlansFolder } from "$lib/api";
  import { app, act, select, toast } from "$lib/state.svelte";
  import Modal from "./Modal.svelte";
  import RunDefaults from "./RunDefaults.svelte";
  import { slug, nameInput, NAME_FIELD } from "$lib/slug";

  let defaults = $state<Defaults | null>(null);
  let folders = $state<PlansFolder[]>([]);
  let name = $state("");
  let group = $state(app.newAgentGroup);
  let cwd = $state("");
  let cwdTouched = $state(false);
  let ticket = $state("");
  let createTicket = $state(false);
  let ticketNotes = $state("");
  let task = $state("");
  let promptFile = $state("");
  let backend = $state("");
  let harness = $state("");
  let busy = $state(false);

  // Start from a new session, or continue an existing Claude Code session.
  let source = $state<"new" | "existing">("new");
  let sessions = $state<ClaudeSession[]>([]);
  let sessionId = $state("");
  let session = $state<ClaudeSession | null>(null);
  let sessionError = $state("");
  let fork = $state(false);

  const groupInfo = $derived(app.snap.groups.find((g) => g.name === group));
  // A group's prompts live in its plans folder; a solo agent's in the root.
  const folder = $derived(groupInfo ? groupInfo.plans_dir || groupInfo.name : "");
  const prompts = $derived(folders.find((f) => f.name === folder)?.prompts ?? []);

  onMount(async () => {
    try {
      [defaults, folders] = await Promise.all([api.defaults(), api.plansFolders()]);
    } catch (e) {
      toast(String(e));
    }
  });

  // The working directory follows the session, else the group, else the
  // default, until you edit it. A resumed session must run where it started.
  $effect(() => {
    if (!cwdTouched) cwd = (source === "existing" && session?.cwd) || groupInfo?.cwd || defaults?.cwd || "";
  });

  $effect(() => {
    if (source === "existing" && !sessions.length) api.claudeSessions().then((s) => (sessions = s)).catch((e) => toast(String(e)));
  });

  // Look the session up as you pick or paste its ID.
  $effect(() => {
    const id = sessionId.trim();
    session = null;
    sessionError = "";
    if (source !== "existing" || !id) return;
    const known = sessions.find((s) => s.id === id);
    if (known) {
      session = known;
      return;
    }
    api.findSession(id).then((s) => {
      if (sessionId.trim() !== id) return;
      session = s;
      sessionError = s ? "" : "No Claude Code session with that ID in ~/.claude/projects";
    }).catch((e) => { if (sessionId.trim() === id) sessionError = String(e); });
  });

  $effect(() => {
    if (session && !name) name = slug(session.cwd.split("/").pop() ?? "");
  });

  const resuming = $derived(source === "existing");
  // The same rule as drovr-core's valid_name, so a bad name shows before Start.
  const nameOk = $derived(/^[a-z][a-z0-9_-]{0,39}$/.test(name));
  const nameTaken = $derived(app.snap.agents.some((a) => a.name === name));
  const ready = $derived(nameOk && !nameTaken && !!backend && (resuming ? !!session : !!task.trim()));

  $effect(() => {
    folder;
    promptFile = "";
  });

  async function loadPrompt() {
    if (!promptFile) return;
    try {
      task = await api.readPrompt(folder, promptFile);
      // Agent names are global, so a group's agents start with the group name.
      if (!name) {
        const base = slug(promptFile.replace(/\.(md|txt)$/i, ""));
        name = group && !base.startsWith(`${group}-`) ? `${group}-${base}` : base;
      }
    } catch (e) {
      toast(String(e));
    }
  }

  function close() {
    app.newAgentOpen = false;
    app.newAgentGroup = "";
  }

  async function go() {
    busy = true;
    const create = createTicket && !ticket.trim();
    const spec = { name, group, cwd, ticket, task, backend, harness, resume_session: resuming ? sessionId.trim() : "", fork: resuming && fork,
      create_ticket: create, ticket_notes: create ? ticketNotes : "" };
    const a = await act(() => api.newAgent(spec), resuming ? `${name} picked up the session` : `${name} started`);
    busy = false;
    if (a) { close(); select(a.name); }
  }
</script>

<Modal title="New agent" onclose={close} dirty={!!task.trim() || !!ticket.trim() || !!ticketNotes.trim()}>
  <div class="source">
    <button class:on={source === "new"} onclick={() => (source = "new")}>New session</button>
    <button class:on={source === "existing"} onclick={() => (source = "existing")}>Existing Claude session</button>
  </div>

  {#if resuming}
    <label for="n-session">Session</label>
    <select id="n-session" bind:value={sessionId}>
      <option value="">{sessions.length ? "Recent sessions…" : "Loading recent sessions…"}</option>
      {#each sessions as s}
        <option value={s.id}>{ago(s.modified)} ago · {s.cwd.split("/").slice(-2).join("/")} · {s.first_prompt || s.id}</option>
      {/each}
    </select>
    <input class="sid" bind:value={sessionId} placeholder="…or paste a session ID (claude --resume <id>)" spellcheck="false" />
    {#if session}
      <p class="muted small">Runs in <code>{session.cwd}</code>{session.first_prompt ? ` · "${session.first_prompt}"` : ""}</p>
    {:else if sessionError}
      <p class="error small">{sessionError}</p>
    {/if}
    <label class="check"><input type="checkbox" bind:checked={fork} /> Fork: continue in a copy and leave the original session untouched</label>
    <p class="muted small">Use Fork if the session is still open in a terminal or herdr pane; two processes writing to the same session would interleave.</p>
  {/if}

  <label for="n-name">Name</label>
  <input id="n-name" value={name} oninput={(e) => (name = nameInput(e))} {...NAME_FIELD} placeholder="fix-login" aria-invalid={!!name && (!nameOk || (nameTaken && !busy))} aria-describedby="n-name-hint" />
  {#if name && !nameOk}
    <p id="n-name-hint" class="error small">Up to 40 lowercase letters, digits, - and _, starting with a letter.</p>
  {:else if nameTaken && !busy}
    <p id="n-name-hint" class="error small">An agent named {name} already exists.</p>
  {/if}

  <label for="n-group">Group</label>
  <select id="n-group" bind:value={group}>
    <option value="">None (solo agent)</option>
    {#each app.snap.groups as g}<option value={g.name}>{g.name}{g.plans_dir ? ` · plans/${g.plans_dir}` : ""}</option>{/each}
  </select>

  <label for="n-cwd">Working directory</label>
  <input id="n-cwd" bind:value={cwd} oninput={() => (cwdTouched = true)} />

  <label for="n-prompt">Prompt from file <span class="where">in {defaults ? `${defaults.plans_dir}${folder ? `/${folder}` : ""}` : "plans"}</span></label>
  <select id="n-prompt" bind:value={promptFile} onchange={loadPrompt} disabled={!prompts.length}>
    <option value="">{prompts.length ? "Choose a file…" : "No .md or .txt files there"}</option>
    {#each prompts as p}<option value={p}>{p}</option>{/each}
  </select>

  <label for="n-task">Task <span class="where">{resuming ? "optional: what to do next" : "the agent's first prompt"}</span></label>
  <textarea id="n-task" rows={resuming ? 4 : 8} bind:value={task}
    placeholder={resuming ? "Leave empty to continue where the session left off" : "What should be done, constraints, definition of done…"}></textarea>

  <label for="n-ticket">Jira ticket (optional)</label>
  <input id="n-ticket" bind:value={ticket} placeholder="SRE-123" disabled={createTicket} />
  <label class="check"><input type="checkbox" bind:checked={createTicket} disabled={!!ticket.trim()} /> Create a Jira ticket for this</label>
  {#if createTicket}
    <input id="n-ticket-notes" bind:value={ticketNotes} aria-label="What you want in the ticket" placeholder="Optional: a bug, assigned to me, in the current sprint" />
    <p class="muted small">The agent writes the ticket first{defaults?.ticket_skill ? ` with ${defaults.ticket_skill}` : ""}, and creates it once you allow the command. Its key becomes the agent's ticket.</p>
  {/if}

  <RunDefaults {defaults} bind:backend bind:harness id="n-backend" claudeOnly={resuming} />

  {#snippet footer()}
    <button onclick={close}>Cancel</button>
    <button class="primary" disabled={busy || !ready} onclick={go}>{busy ? "Starting…" : "Start"}</button>
  {/snippet}
</Modal>

<style>
  .where { text-transform: none; letter-spacing: 0; opacity: 0.8; margin-left: 4px; }
  .source { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
  .source button.on { border-color: var(--accent); color: var(--accent); }
  .sid { margin-top: 6px; font-family: var(--mono); font-size: 12px; }
  .small { font-size: 12px; margin: 4px 0 0; }
  .error { color: var(--red); }
  .check { display: flex; align-items: center; gap: 8px; text-transform: none; letter-spacing: 0; font-size: 13px; color: var(--text); margin-top: 10px; }
  .check input { width: auto; }
</style>
