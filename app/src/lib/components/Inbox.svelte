<script lang="ts">
  import { api, ago, type Item } from "#lib/api.ts";
  import { app, act, select, toast } from "#lib/state.svelte.ts";
  import { md } from "#lib/md.ts";
  import { shq } from "#lib/terminal.svelte.ts";

  let showResolved = $state(false);
  let drafts = $state<Record<number, string>>({});
  let commands = $state<Record<number, string>>({});
  let running = $state<Record<number, boolean>>({});

  /** Answer an item once: its buttons are disabled until the answer lands. */
  async function once(id: number, fn: () => Promise<unknown>, ok?: string) {
    if (running[id]) return;
    running[id] = true;
    await act(fn, ok);
    running[id] = false;
  }

  const kinds: Record<string, string> = {
    decision: "Decision", command: "Command", permission: "Permission", spawn: "Spawn", done: "Finished",
    watch: "Watcher", jira: "Jira draft", handoff: "Handoff", confluence: "Confluence",
  };
  const order = ["permission", "decision", "handoff", "command", "spawn", "watch", "jira", "confluence", "done"];
  let models = $state<Record<number, string>>({});

  /** Approve a stage's handoff; the next stage runs on the model you leave in the box. */
  function approveHandoff(item: Item) {
    const model = (models[item.id] ?? item.payload.next_model ?? "").trim();
    return once(item.id, () => api.answer(item.id, model ? `Approve model=${model}` : "Approve"), item.payload.next ? `Approved; the lead starts ${item.payload.next}` : "Approved; the stages are done");
  }

  /** Post a Jira draft as you edited it: the comment, or the status to move to. */
  function postJira(item: Item) {
    const text = drafts[item.id] ?? (item.payload.action === "comment" ? item.payload.body : item.payload.transition);
    return once(item.id, () => api.answer(item.id, text), item.payload.action === "comment" ? "Comment posted to Jira" : "Ticket moved in Jira");
  }
  const items = $derived([...app.snap.inbox].sort((a, b) => order.indexOf(a.kind) - order.indexOf(b.kind) || a.id - b.id));

  /** Type the command into the agent's terminal, for you to check and run. */
  function runInTerminal(item: Item) {
    const cmd = commands[item.id] ?? item.payload.command;
    const cwd = item.payload.cwd as string | undefined;
    app.selected = item.agent;
    app.view = "agents";
    app.termOpen = true;
    app.termRun = { agent: item.agent, itemId: item.id, text: cwd ? `cd ${shq(cwd)} && ${cmd}` : cmd };
  }

  /** Tell the agent you ran it, with your note, or else what its terminal printed. */
  async function markRan(item: Item) {
    let note = (drafts[item.id] ?? "").trim();
    const m = app.termMarks[item.id];
    if (!note && m) {
      const out = await api.termSince(m.key, m.mark, 20000).catch(() => "");
      if (out) note = out;
    }
    await once(item.id, () => api.markRan(item.id, note), "Marked as run; the agent was told");
    delete app.termMarks[item.id];
  }

  // The answers the buttons and the keyboard share.
  const allow = (item: Item) => once(item.id, () => api.answer(item.id, "allow"));
  const deny = (item: Item) => once(item.id, () => api.answer(item.id, drafts[item.id] ? `deny: ${drafts[item.id]}` : "deny"));
  const startWorker = (item: Item) => once(item.id, () => api.answer(item.id, "approve"), "Worker started");
  const declineWorker = (item: Item) =>
    once(item.id, () => api.reject(item.id, `${app.you.charAt(0).toUpperCase()}${app.you.slice(1)} declined this worker.`));
  const pick = (item: Item, opt: string) => once(item.id, () => api.answer(item.id, opt), "Answer sent");

  // Keyboard: ↓↑ (or J K) move between items, 1–9 pick a decision's
  // option, A allows or starts, D denies or declines. Not while typing.
  let at = $state(0);
  const current = $derived(items.length ? items[Math.min(at, items.length - 1)] : undefined);
  let cards: Record<number, HTMLElement> = {};

  function onKey(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return;
    if ((e.target as HTMLElement | null)?.closest?.("input, textarea, select, [contenteditable], .modal")) return;
    if (app.newAgentOpen || app.newGroupOpen || app.dispatching || app.shortcutsOpen) return;
    const item = current;
    if (!item) return;
    const k = e.key.toLowerCase();
    if (k === "arrowdown" || k === "j" || k === "arrowup" || k === "k") {
      const step = k === "arrowdown" || k === "j" ? 1 : -1;
      at = Math.max(0, Math.min(items.length - 1, items.indexOf(item) + step));
      cards[items[at].id]?.scrollIntoView({ block: "nearest" });
    } else if (/^[1-9]$/.test(k) && item.options[Number(k) - 1] && item.kind !== "permission") {
      pick(item, item.options[Number(k) - 1]);
    } else if (k === "a" && item.kind === "permission") allow(item);
    else if (k === "d" && item.kind === "permission") deny(item);
    else if (k === "a" && item.kind === "spawn") startWorker(item);
    else if (k === "d" && item.kind === "spawn") declineWorker(item);
    else return;
    e.preventDefault();
  }

  function run(item: Item, target: string) {
    return once(item.id, () => api.runCommand(item.id, target, commands[item.id] ?? item.payload.command), target === "herdr" ? "Running in herdr" : "Command finished; output sent to the agent");
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="page scroll">
  <div class="row top">
    <h2 class="grow">Inbox</h2>
    {#if items.length}<span class="muted keys">↓↑ move · 1–9 answer · A allow · D deny</span>{/if}
    <label class="row check"><input type="checkbox" bind:checked={showResolved} /> show resolved</label>
  </div>

  {#each items as item (item.id)}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <article class="card k-{item.kind}" class:busy={running[item.id]} class:at={item.id === current?.id} aria-busy={running[item.id] ? "true" : undefined}
      aria-current={item.id === current?.id ? "true" : undefined} bind:this={cards[item.id]} onclick={() => (at = items.indexOf(item))}>
      <div class="row">
        <span class="pill">{kinds[item.kind] ?? item.kind}</span>
        <button class="ghost agent" onclick={() => select(item.agent)}>{item.agent}</button>
        <span class="muted grow">#{item.id} · {ago(item.ts)} ago</span>
        <button class="ghost" title="Dismiss without answering" onclick={() => once(item.id, () => api.dismiss(item.id))}>✕</button>
      </div>

      {#if item.kind === "command"}
        <div class="reason md">{@html md(item.body)}</div>
        <textarea rows="2" value={commands[item.id] ?? item.payload.command} oninput={(e) => (commands[item.id] = e.currentTarget.value)}></textarea>
        <div class="muted small">in {item.payload.cwd}</div>
        <div class="row actions">
          <button class="primary" disabled={running[item.id]} onclick={() => run(item, "local")}>{running[item.id] ? "Running…" : "Run & return output"}</button>
          <button onclick={() => runInTerminal(item)} title="Type it into the agent's terminal in drovr; you press Enter">Run in terminal</button>
          {#if app.snap.herdr}<button disabled={running[item.id]} onclick={() => run(item, "herdr")}>Run in herdr pane</button>{/if}
          <button onclick={() => navigator.clipboard.writeText(commands[item.id] ?? item.payload.command).then(() => toast("Copied. Mark as run when you've run it."))}>Copy</button>
          <input class="grow" placeholder="Output or note for Mark as run, or reason to reject (optional)" bind:value={drafts[item.id]} />
          <button title="You ran it yourself; tells the agent, with your note, or else what its terminal printed after Run in terminal" onclick={() => markRan(item)}>Mark as run</button>
          <button class="danger" onclick={() => once(item.id, () => api.reject(item.id, drafts[item.id] ?? ""))}>Reject</button>
        </div>
      {:else if item.kind === "permission"}
        <h3><code>{item.title}</code></h3>
        <div class="md">{@html md(item.body)}</div>
        <div class="row actions">
          <button class="primary" onclick={() => allow(item)}>Allow <span class="kbd">A</span></button>
          <input class="grow" placeholder="Tell it why, or what to do instead (optional)" bind:value={drafts[item.id]} />
          <button class="danger" onclick={() => deny(item)}>Deny <span class="kbd">D</span></button>
        </div>
      {:else if item.kind === "spawn"}
        <h3>{item.title}</h3>
        <div class="md">{@html md(item.body)}</div>
        <div class="row actions">
          <button class="primary" onclick={() => startWorker(item)}>Start worker <span class="kbd">A</span></button>
          <button class="danger" onclick={() => declineWorker(item)}>Decline <span class="kbd">D</span></button>
        </div>
      {:else if item.kind === "handoff"}
        <h3>{item.title}</h3>
        <details class="handoff" open><summary class="muted small">{item.payload.file}</summary><div class="md">{@html md(item.body)}</div></details>
        <div class="row actions">
          <button class="primary" disabled={running[item.id]} onclick={() => approveHandoff(item)}>Approve</button>
          {#if item.payload.next}
            <span class="muted small">{item.payload.next} runs on</span>
            <input style="max-width: 200px" placeholder="the default model" value={models[item.id] ?? item.payload.next_model ?? ""} oninput={(e) => (models[item.id] = e.currentTarget.value)} />
          {/if}
          <input class="grow" placeholder="Notes for sending it back" bind:value={drafts[item.id]} />
          <button class="danger" onclick={() => once(item.id, () => api.reject(item.id, drafts[item.id] ?? ""), "Sent back with your notes")}>Send back</button>
        </div>
      {:else if item.kind === "jira"}
        <h3>{item.title}</h3>
        {#if item.payload.action === "comment"}
          <textarea rows="5" value={drafts[item.id] ?? item.payload.body} oninput={(e) => (drafts[item.id] = e.currentTarget.value)}></textarea>
        {:else}
          <div class="row"><span class="muted">Move {item.payload.ticket} to</span>
            <input value={drafts[item.id] ?? item.payload.transition} oninput={(e) => (drafts[item.id] = e.currentTarget.value)} style="max-width: 240px" /></div>
          {#if item.payload.fields}<div class="muted small">with {item.payload.fields}</div>{/if}
        {/if}
        <div class="muted small">Nothing is posted until you do. drovr runs your <code>jira.{item.payload.action === "comment" ? "comment_cmd" : "transition_cmd"}</code>.</div>
        <div class="row actions">
          <button class="primary" disabled={running[item.id]} onclick={() => postJira(item)}>{running[item.id] ? "Posting…" : "Post to Jira"}</button>
          <button onclick={() => once(item.id, () => api.dismiss(item.id))}>Discard</button>
        </div>
      {:else if item.kind === "confluence"}
        <h3>{item.title}</h3>
        <div class="md">{@html md(item.body)}</div>
        {#if item.payload.action === "update"}
          <details open><summary class="muted small">Changes from the live draft</summary>
            {#if item.payload.diff}
              <pre class="diff">{#each item.payload.diff.split("\n") as l}<span class:add={l.startsWith("+")} class:del={l.startsWith("-")} class:hunk={l.startsWith("@@")}>{l}</span>
{/each}</pre>
            {:else}<div class="muted small">No changes from the live draft.</div>{/if}
          </details>
        {/if}
        <details open><summary class="muted small">The full page</summary>
          <!-- sandbox="": the agent's HTML can't run scripts or reach drovr. -->
          <iframe class="confluence" sandbox="" title={`Preview of ${item.payload.title}`} srcdoc={item.payload.html}></iframe>
        </details>
        <div class="row actions">
          <button class="primary" disabled={running[item.id]} onclick={() => once(item.id, () => api.answer(item.id, "approve"), "Saved as a draft in Confluence")}>{running[item.id] ? "Saving…" : "Save as draft in Confluence"}</button>
          <input class="grow" placeholder="Notes for sending it back" bind:value={drafts[item.id]} />
          <button class="danger" onclick={() => once(item.id, () => api.reject(item.id, drafts[item.id] ?? ""), "Sent back with your notes")}>Send back</button>
        </div>
        <div class="muted small">drovr reads the page again before saving: if it changed since this diff, you get the new diff instead. Nothing is published.</div>
      {:else if item.kind === "done"}
        <h3>{item.title}</h3>
        <div class="md">{@html md(item.body)}</div>
        <div class="row actions">
          <button onclick={() => { app.selected = item.agent; app.view = "review"; }}>Review changes</button>
          <button onclick={() => once(item.id, () => api.dismiss(item.id))}>Acknowledge</button>
        </div>
      {:else}
        <h3>{item.title}</h3>
        {#if item.body}<div class="md">{@html md(item.body)}</div>{/if}
        {#if item.options.length}
          <div class="options">
            {#each item.options as opt, i}
              <button onclick={() => pick(item, opt)}><span class="muted">{i + 1}</span> {opt}</button>
            {/each}
          </div>
        {/if}
        <div class="row actions">
          <input class="grow" placeholder="Or write an answer…" bind:value={drafts[item.id]}
            onkeydown={(e) => e.key === "Enter" && drafts[item.id] && once(item.id, () => api.answer(item.id, drafts[item.id]), "Answer sent")} />
          <button class="primary" disabled={!drafts[item.id]} onclick={() => once(item.id, () => api.answer(item.id, drafts[item.id]), "Answer sent")}>Answer</button>
        </div>
      {/if}
    </article>
  {:else}
    <div class="empty">Nothing needs you right now.</div>
  {/each}

  {#if showResolved}
    <h3 class="muted">Recently resolved</h3>
    {#each app.snap.recent as item}
      <div class="resolved">
        <span class="pill">{item.kind}</span> <strong>{item.agent}</strong> {item.title}
        <span class="muted">→ {item.status}{item.answer ? `: ${item.answer.split("\n")[0].slice(0, 120)}` : ""}</span>
      </div>
    {/each}
  {/if}
</div>

<style>
  .page { padding: 14px 20px; height: 100%; }
  .top { margin-bottom: 8px; }
  h2 { margin: 0; font-size: 18px; }
  h3 { margin: 8px 0 4px; font-size: 14px; }
  .check { width: auto; text-transform: none; letter-spacing: 0; font-size: 12px; margin: 0; }
  .check input { width: auto; }
  .card { background: var(--panel); border: 1px solid var(--border); border-left: 3px solid var(--accent); border-radius: 8px; padding: 10px 14px; margin-bottom: 10px; max-width: 980px; }
  .card.at { box-shadow: 0 0 0 1px var(--accent); }
  .keys { font-size: 11px; }
  .kbd { opacity: .6; font-size: 10px; margin-left: 4px; }
  .k-permission { border-left-color: var(--yellow); }
  .k-command { border-left-color: var(--cyan); }
  .k-spawn { border-left-color: var(--purple); }
  .k-done { border-left-color: var(--green); }
  .k-watch { border-left-color: var(--cyan); }
  .k-jira { border-left-color: var(--purple); }
  .k-handoff { border-left-color: var(--green); }
  .k-confluence { border-left-color: var(--accent); }
  .diff { max-height: 320px; overflow: auto; font-size: 12px; background: var(--bg); padding: 8px; border-radius: 6px; white-space: pre-wrap; }
  .diff .add { color: var(--green); }
  .diff .del { color: var(--red); }
  .diff .hunk { color: var(--muted); }
  iframe.confluence { width: 100%; height: 420px; border: 1px solid var(--border); border-radius: 6px; background: #fff; }
  .handoff { margin: 4px 0; max-height: 360px; overflow: auto; }
  .agent { font-weight: 600; padding: 2px 6px; }
  .reason { margin: 6px 0; }
  .options { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0; }
  .options button { text-align: left; }
  .actions { margin-top: 8px; flex-wrap: wrap; }
  .actions input { min-width: 200px; }
  .small { font-size: 11px; margin-top: 2px; }
  .busy :is(.actions, .options) button { opacity: .45; pointer-events: none; }
  .resolved { font-size: 12px; padding: 3px 0; border-bottom: 1px solid var(--border); }
</style>
