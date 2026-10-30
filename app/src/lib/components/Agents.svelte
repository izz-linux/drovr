<script lang="ts">
  import { api, ago, ticketsIn, type Agent, type Line } from "#lib/api.ts";
  import { app, act, toast } from "#lib/state.svelte.ts";
  import { md } from "#lib/md.ts";
  import TerminalPanel from "./TerminalPanel.svelte";

  let lines = $state<Line[]>([]);
  let input = $state("");
  let filter = $state("");
  let follow = $state(true);
  let logEl: HTMLDivElement | undefined = $state();

  // Find in the agent's output (Edit → Find…, ⌘F).
  let findOpen = $state(false);
  let findQ = $state("");
  let findAt = $state(0);
  let findEl: HTMLInputElement | undefined = $state();
  const hits = $derived.by(() => {
    const q = findQ.trim().toLowerCase();
    if (!q) return [] as number[];
    return lines.flatMap((l, i) => (l.text.toLowerCase().includes(q) ? [i] : []));
  });
  const hitLine = $derived(hits.length ? hits[Math.min(findAt, hits.length - 1)] : -1);

  let lastFind = app.find;
  $effect(() => {
    if (app.find === lastFind) return;
    lastFind = app.find;
    findOpen = true;
    queueMicrotask(() => { findEl?.focus(); findEl?.select(); });
  });

  function jump(step: number) {
    if (!hits.length) return;
    findAt = (findAt + step + hits.length) % hits.length;
    follow = false;
    queueMicrotask(() => logEl?.querySelector(`[data-i="${hits[findAt]}"]`)?.scrollIntoView({ block: "center" }));
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === "Enter") { e.preventDefault(); jump(e.shiftKey ? -1 : 1); }
    if (e.key === "Escape") { findOpen = false; findQ = ""; }
  }

  // Collapsed groups, remembered in this browser profile. "" is the Solo section.
  const COLLAPSED_KEY = "drovr.collapsedGroups";
  let collapsed = $state<Record<string, boolean>>(loadCollapsed());

  function loadCollapsed(): Record<string, boolean> {
    try {
      return JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "{}");
    } catch {
      return {};
    }
  }

  function setCollapsed(next: Record<string, boolean>) {
    collapsed = next;
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify(next));
    } catch {}
  }

  const toggle = (group: string) => setCollapsed({ ...collapsed, [group]: !collapsed[group] });
  // While filtering, show every match even in collapsed groups.
  const isCollapsed = (group: string) => !filter && !!collapsed[group];

  function summary(list: Agent[]): { working: number; blocked: number } {
    return {
      working: list.filter((a) => a.state === "working").length,
      blocked: list.filter((a) => a.state === "blocked").length,
    };
  }

  const agents = $derived(
    app.snap.agents.filter((a) => !filter || `${a.name} ${a.group_name} ${a.ticket} ${a.status}`.toLowerCase().includes(filter.toLowerCase())),
  );
  const grouped = $derived.by(() => {
    const out: { group: string; agents: Agent[] }[] = [];
    for (const a of agents) {
      const g = a.group_name || "";
      let entry = out.find((x) => x.group === g);
      if (!entry) out.push((entry = { group: g, agents: [] }));
      entry.agents.push(a);
    }
    // Groups you created that have no agents yet still show, so you can add some.
    if (!filter) {
      for (const g of app.snap.groups) {
        if (!out.some((x) => x.group === g.name)) out.push({ group: g.name, agents: [] });
      }
    }
    return out.sort((a, b) => (a.group === "" ? 1 : b.group === "" ? -1 : a.group.localeCompare(b.group)));
  });
  const allCollapsed = $derived(grouped.length > 0 && grouped.every((g) => collapsed[g.group]));
  const current = $derived(app.snap.agents.find((a) => a.name === app.selected));
  // defaults.cost_warning_usd: flag agents that have cost more than this.
  const costCap = $derived(app.defaults?.cost_warning_usd ?? 0);
  const overBudget = (a: Agent) => costCap > 0 && a.cost_usd > costCap;
  const interrupted = $derived(app.snap.agents.filter((a) => a.state === "interrupted"));

  async function resumeAll() {
    for (const a of interrupted) await act(() => api.resume(a.name));
  }
  const pending = $derived(app.snap.inbox.filter((i) => i.agent === app.selected && i.kind !== "done"));

  // The tickets an agent's record is kept for: its own, else its group's.
  function ticketsOf(a: Agent): string[] {
    const own = ticketsIn(a.ticket);
    return own.length ? own : ticketsIn(app.snap.groups.find((g) => g.name === a.group_name)?.ticket ?? "");
  }
  // Only with the ticket workflow on (workflow.enabled).
  const currentTickets = $derived(current && app.defaults?.workflow ? ticketsOf(current) : []);

  $effect(() => {
    if (!app.selected && app.snap.agents.length) app.selected = app.snap.agents[0].name;
  });

  // Poll the selected agent's output. Unchanged output isn't re-rendered,
  // since every text line goes through the Markdown renderer.
  $effect(() => {
    const name = app.selected;
    lines = [];
    if (!name) return;
    let alive = true;
    let last = "";
    const load = async () => {
      try {
        const l = await api.output(name, 800);
        const sig = JSON.stringify(l);
        if (alive && sig !== last) { last = sig; lines = l; }
      } catch {}
    };
    load();
    const t = setInterval(load, 1500);
    return () => { alive = false; clearInterval(t); };
  });

  $effect(() => {
    lines.length;
    if (follow && logEl) queueMicrotask(() => logEl && (logEl.scrollTop = logEl.scrollHeight));
  });

  async function send() {
    if (!current || !input.trim()) return;
    const text = input;
    input = "";
    const r = await act(() => api.prompt(current.name, text));
    // On an error, give the text back instead of losing it.
    if (r === undefined && !input) input = text;
  }

  // A queued message shows above the box until the agent reads it. Send now
  // stops its run and starts one with the message; ✕ takes it back.
  async function sendNow(id: number) {
    if (!current) return;
    const name = current.name;
    await act(() => api.sendNow(name, id), current.state === "working" ? `Stopped ${name}'s run and sent it` : `Sent to ${name}`);
  }

  async function unqueue(id: number, body: string) {
    if (!current) return;
    const took = await act(() => api.unqueue(current!.name, id));
    if (took === false) toast(`${current.name} has already read it`);
    // Back in the box to edit, unless you've started typing something else.
    else if (took && !input.trim()) input = body;
  }

  // defaults.copy_on_select: selecting text in the output copies it.
  function copySelection() {
    if (!(app.defaults?.copy_on_select ?? true)) return;
    const sel = window.getSelection();
    const text = sel?.toString() ?? "";
    if (!text.trim() || !sel?.anchorNode || !logEl?.contains(sel.anchorNode)) return;
    navigator.clipboard.writeText(text).catch(() => {});
  }
  $effect(() => {
    const el = logEl;
    el?.addEventListener("mouseup", copySelection);
    return () => el?.removeEventListener("mouseup", copySelection);
  });

  async function removeGroup(group: string) {
    const note = await act(() => api.removeGroup(group));
    if (note === undefined) return;
    toast(note ? `Removed ${group}.\n${note}` : `Removed ${group}`);
    if (!app.snap.agents.some((a) => a.name === app.selected)) app.selected = app.snap.agents[0]?.name ?? "";
  }

  async function removeAgent(name: string) {
    const note = await act(() => api.remove(name));
    if (note === undefined) return;
    toast(note ? `Removed ${name}. ${note}` : `Removed ${name}`);
    if (app.selected === name) app.selected = app.snap.agents[0]?.name ?? "";
  }

  async function attach() {
    if (!current) return;
    if (app.snap.herdr) {
      await act(() => api.openInHerdr(current.name), `Opened ${current.name} in a herdr pane`);
    } else {
      const cmd = await api.attachCommand(current.name);
      await navigator.clipboard.writeText(cmd);
      toast("Copied a command to open this session in your terminal");
    }
  }

  // Enter sends; Shift+Enter adds a new line. Enter while an input method is
  // composing (e.g. picking a Japanese candidate) belongs to the IME.
  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
  }

  // window.confirm() never shows in the macOS app window (it just returns
  // false), so removing asks with a second click instead.
  let arming = $state("");
  let armTimer: ReturnType<typeof setTimeout> | undefined;
  function twice(key: string, run: () => void) {
    if (arming === key) { arming = ""; clearTimeout(armTimer); run(); return; }
    arming = key;
    clearTimeout(armTimer);
    armTimer = setTimeout(() => (arming = ""), 4000);
  }
</script>

{#snippet counts(list: Agent[], closed: boolean)}
  {#if closed}
    {@const c = summary(list)}
    <span class="muted count">{list.length}</span>
    {#if c.blocked}<span class="pill s-blocked">{c.blocked} blocked</span>{/if}
    {#if c.working}<span class="pill s-working">{c.working} working</span>{/if}
  {/if}
{/snippet}

<div class="wrap">
  <section class="list">
    <div class="head">
      <input placeholder="Filter agents" bind:value={filter} />
      <button class="ghost" title={allCollapsed ? "Expand all groups" : "Collapse all groups"}
        onclick={() => setCollapsed(Object.fromEntries(grouped.map((g) => [g.group, !allCollapsed])))}>{allCollapsed ? "⊞" : "⊟"}</button>
    </div>
    {#if interrupted.length}
      <div class="interrupted">
        <span class="grow">{interrupted.length} agent{interrupted.length > 1 ? "s were" : " was"} interrupted by a restart</span>
        <button onclick={resumeAll}>Resume all</button>
      </div>
    {/if}
    <div class="scroll grow">
      {#each grouped as g}
        {#if g.group}
          {@const group = app.snap.groups.find((x) => x.name === g.group)}
          <div class="group">
            <div class="row ghead">
              <button class="toggle grow" onclick={() => toggle(g.group)} aria-expanded={!isCollapsed(g.group)} title={g.group}>
                <span class="caret">{isCollapsed(g.group) ? "▸" : "▾"}</span><strong class="gname">{g.group}</strong>
                {@render counts(g.agents, isCollapsed(g.group))}
              </button>
              <!-- The actions stay together; in a narrow list they go under the name. -->
              <div class="row gactions">
              <button class="ghost small" title="Add an agent to this group" onclick={() => { app.newAgentGroup = g.group; app.newAgentOpen = true; }}>+ agent</button>
              {#if app.defaults?.workflow && group && ticketsIn(group.ticket).length}
                <button class="ghost small" title={`Timeline of ${group.ticket}`} onclick={() => (app.timeline = ticketsIn(group.ticket)[0])}>timeline</button>
              {/if}
              {#if !g.agents.some((a) => a.role === "orchestrator")}
                <button class="ghost small" title="Start an orchestrator for this group" onclick={() => act(() => api.addLead(g.group, "", ""), `Started ${g.group}-lead`)}>+ lead</button>
              {/if}
              <button class="ghost small" title="Stop every agent in this group" onclick={() => act(() => api.stopGroup(g.group), `Stopped ${g.group}`)}>stop all</button>
              <button class="ghost small danger" class:armed={arming === `g:${g.group}`} title="Stop and remove the group and its agents"
                onclick={() => twice(`g:${g.group}`, () => removeGroup(g.group))}>{arming === `g:${g.group}` ? "Remove?" : "✕"}</button>
              </div>
            </div>
            {#if !isCollapsed(g.group)}
              {#if group?.ticket}<div class="muted small indent">{group.ticket}{#if app.defaults?.workflow && group.workflow}<span title="Created with the ticket workflow on: its lead runs the ticket in stages">{" · in stages"}</span>{/if}</div>{/if}
              {#if group?.plans_dir}<div class="muted small indent" title="Plans folder">plans/{group.plans_dir}</div>{/if}
              {#if !g.agents.length}<div class="muted small indent">No agents yet.</div>{/if}
            {/if}
          </div>
        {:else if grouped.length > 1}
          <div class="group">
            <button class="toggle" onclick={() => toggle("")} aria-expanded={!isCollapsed("")}>
              <span class="caret">{isCollapsed("") ? "▸" : "▾"}</span><strong>Solo</strong>
              {@render counts(g.agents, isCollapsed(""))}
            </button>
          </div>
        {/if}
        {#each isCollapsed(g.group) ? [] : g.agents as a}
          <button class="agent" class:sel={a.name === app.selected} class:lead={a.role === "orchestrator"} onclick={() => (app.selected = a.name)}>
            <div class="row">
              <span class="name grow">{a.role === "orchestrator" ? "★ " : ""}{a.name}</span>
              {#if a.stage}<span class="pill stage" title="Its stage in the ticket workflow">{a.stage}</span>{/if}
              {#if overBudget(a)}<span class="pill cost" title={`Over your cost warning of $${costCap} (defaults.cost_warning_usd)`}>${a.cost_usd.toFixed(2)}</span>{/if}
              <span class="pill s-{a.state}">{a.state}</span>
            </div>
            <div class="muted status">{a.status || a.task.split("\n")[0]}</div>
          </button>
        {/each}
      {:else}
        <div class="empty">No agents yet.<br />Start one, or dispatch a Jira ticket.</div>
      {/each}
    </div>
  </section>

  <section class="detail">
    {#if current}
      <header>
        <div class="row">
          <h2 class="grow">{current.name}</h2>
          <span class="pill s-{current.state}">{current.state}</span>
          {#if current.state === "working"}
            <button class="danger" onclick={() => act(() => api.stop(current.name), "Stopped")}>Stop</button>
          {:else if current.state === "stopped" || current.state === "error" || current.state === "interrupted"}
            <button onclick={() => act(() => api.resume(current.name), "Resumed")}>Resume</button>
          {/if}
          <button onclick={attach} title="Open this session interactively">{app.snap.herdr ? "Open in herdr" : "Copy attach cmd"}</button>
          <button onclick={() => { app.view = "review"; }}>Review</button>
          {#if currentTickets.length}
            <button onclick={() => (app.timeline = currentTickets[0])} title={`Everything that happened on ${currentTickets.join(", ")}, for review and the retro`}>Timeline</button>
          {/if}
          <button class:on={app.termOpen} onclick={() => (app.termOpen = !app.termOpen)} title="Show or hide the terminal (⌃`)">Terminal</button>
          <button class="ghost danger" class:armed={arming === `a:${current.name}`} title="Stop and remove this agent and its output"
            onclick={() => { const n = current!.name; twice(`a:${n}`, () => removeAgent(n)); }}>{arming === `a:${current.name}` ? "Remove?" : "✕"}</button>
        </div>
        <div class="meta muted">
          <span>{current.role}{current.stage ? ` · ${current.stage} stage` : ""}{current.model ? ` · ${current.model}` : ""}</span>
          <span>{current.harness === "codex" ? "Codex · " : ""}{current.backend}{current.pane ? ` · ${current.pane}` : ""}</span>
          {#if current.ticket}<span>{current.ticket}</span>{/if}
          <span title={current.cwd}>{current.cwd.replace(/^.*\/(.+\/.+)$/, "…/$1")}</span>
          <span class:cost={overBudget(current)} title={overBudget(current) ? `Over your cost warning of $${costCap} (defaults.cost_warning_usd)` : "What its headless runs cost, at API list prices"}>{current.runs} runs · ${current.cost_usd.toFixed(2)}{overBudget(current) ? " ⚠" : ""}</span>
          <span>updated {ago(current.updated)} ago</span>
        </div>
        {#if pending.length}
          <button class="needs" onclick={() => (app.view = "inbox")}>⚑ {pending.length} item{pending.length > 1 ? "s" : ""} waiting for you in the Inbox</button>
        {/if}
      </header>

      {#if findOpen}
        <div class="find">
          <input bind:this={findEl} bind:value={findQ} onkeydown={onFindKey} oninput={() => (findAt = 0)} placeholder="Find in output" />
          <span class="muted">{findQ.trim() ? (hits.length ? `${Math.min(findAt, hits.length - 1) + 1} of ${hits.length}` : "No matches") : ""}</span>
          <button class="ghost" onclick={() => jump(-1)} disabled={!hits.length} title="Previous (Shift+Enter)">↑</button>
          <button class="ghost" onclick={() => jump(1)} disabled={!hits.length} title="Next (Enter)">↓</button>
          <button class="ghost" onclick={() => { findOpen = false; findQ = ""; }} title="Close (Esc)">✕</button>
        </div>
      {/if}
      <div class="log scroll" bind:this={logEl} onscroll={() => logEl && (follow = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 40)}>
        {#each lines as l, i}
          <div class="line" data-i={i} class:hit={hits.includes(i)} class:at={i === hitLine}>
          {#if l.kind === "run"}<div class="sep">{l.text}</div>
          {:else if l.kind === "prompt"}<div class="prompt"><div class="md">{@html md(l.text)}</div></div>
          {:else if l.kind === "text"}<div class="text md">{@html md(l.text)}</div>
          {:else if l.kind === "tool"}<div class="tool"><span>⏵</span> {l.text}</div>
          {:else if l.kind === "result"}<details class="result" open={hits.includes(i)}><summary>{l.text.split("\n")[0].slice(0, 140) || "(no output)"}</summary><pre>{l.text}</pre></details>
          {:else if l.kind === "final"}<div class="final">{l.text}</div>
          {:else if l.kind === "error"}<pre class="err">{l.text}</pre>
          {:else}<pre class="raw">{l.text}</pre>{/if}
          </div>
        {:else}
          <div class="empty">No output yet.</div>
        {/each}
      </div>

      {#if current.queued?.length}
        <div class="queue">
          {#each current.queued as m (m.id)}
            <div class="queued">
              <span class="tag">Queued</span>
              <div class="body grow">{m.body}</div>
              <button class="small" onclick={() => sendNow(m.id)}
                title={current.state === "working" ? "Stop the current run and send this now" : "Send this now"}>Send now</button>
              <button class="ghost small" onclick={() => unqueue(m.id, m.body)} title="Take it back: it goes back in the box to edit">✕</button>
            </div>
          {/each}
        </div>
      {/if}
      <footer>
        <textarea rows="3" bind:value={input} onkeydown={onKey}
          placeholder={current.state === "working" ? "It's busy, so this is queued until its run ends. Enter sends, Shift+Enter for a new line" : "Prompt this agent. Enter sends, Shift+Enter for a new line"}></textarea>
        <button class="primary" onclick={send} disabled={!input.trim()}>Send</button>
      </footer>
      {#if app.termOpen}<TerminalPanel agent={current} />{/if}
    {:else}
      <div class="empty">Select an agent.</div>
    {/if}
  </section>
</div>

<style>
  .wrap { display: grid; grid-template-columns: 300px 1fr; height: 100%; min-height: 0; }
  .list { border-right: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .head { display: flex; gap: 6px; padding: 10px; border-bottom: 1px solid var(--border); }
  .group { padding: 10px 12px 4px; color: var(--muted); font-size: 12px; }
  .group strong { color: var(--text); }
  .toggle { display: flex; align-items: center; gap: 6px; background: transparent; border: none; padding: 0; text-align: left; min-width: 0; }
  .ghead { flex-wrap: wrap; row-gap: 2px; }
  .ghead .toggle { flex: 1 1 120px; }
  .gname { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .gactions { gap: 4px; margin-left: auto; flex: none; }
  .toggle:hover:not(:disabled) strong { text-decoration: underline; }
  .caret { width: 10px; color: var(--muted); }
  .count { font-size: 11px; }
  .indent { padding-left: 16px; }
  .find { display: flex; align-items: center; gap: 6px; padding: 6px 16px; border-bottom: 1px solid var(--border); font-size: 12px; }
  .find input { max-width: 320px; }
  .line.hit { background: color-mix(in srgb, var(--yellow) 12%, transparent); border-radius: 4px; }
  .line.at { background: color-mix(in srgb, var(--yellow) 30%, transparent); outline: 1px solid var(--yellow); }
  .interrupted { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--border); font-size: 12px; color: var(--yellow); }
  .small { font-size: 11px; padding: 1px 6px; }
  .agent { display: block; width: 100%; text-align: left; background: transparent; border: none; border-left: 2px solid transparent; border-radius: 0; padding: 7px 12px; }
  .agent.sel { background: var(--panel-2); border-left-color: var(--accent); }
  .agent.lead .name { color: var(--purple); }
  .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; }
  .status { font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .detail { display: flex; flex-direction: column; min-height: 0; min-width: 0; }
  header { padding: 10px 16px; border-bottom: 1px solid var(--border); }
  h2 { margin: 0; font-size: 16px; }
  .meta { display: flex; flex-wrap: wrap; gap: 14px; font-size: 12px; margin-top: 4px; }
  .needs { margin-top: 8px; border-color: var(--yellow); color: var(--yellow); background: transparent; }
  .log { flex: 1; padding: 12px 16px; min-height: 0; }
  .sep { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .06em; border-top: 1px dashed var(--border); margin: 14px 0 6px; padding-top: 6px; }
  .prompt { border-left: 3px solid var(--purple); padding: 4px 10px; background: var(--panel); border-radius: 4px; margin: 6px 0; }
  .text { margin: 6px 0; }
  .tool { font-family: var(--mono); font-size: 12px; color: var(--cyan); margin: 2px 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .result { font-size: 12px; color: var(--muted); margin: 0 0 4px 16px; }
  .result summary { cursor: pointer; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-family: var(--mono); }
  .result pre { white-space: pre-wrap; max-height: 300px; overflow: auto; background: var(--panel); padding: 6px; border-radius: 4px; }
  .final { color: var(--green); font-size: 12px; margin: 6px 0; }
  .err { color: var(--red); white-space: pre-wrap; }
  .raw { margin: 0; white-space: pre-wrap; }
  .queue { border-top: 1px solid var(--border); padding: 6px 16px 0; display: flex; flex-direction: column; gap: 4px; max-height: 30%; overflow: auto; }
  .queued { display: flex; align-items: flex-start; gap: 8px; padding: 6px 8px; background: var(--panel); border: 1px dashed var(--border); border-radius: 6px; font-size: 13px; }
  .queued .tag { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .06em; padding-top: 2px; }
  .queued .body { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 4.5em; overflow: hidden; }
  footer { display: flex; gap: 8px; padding: 10px 16px; border-top: 1px solid var(--border); align-items: flex-end; }
  .cost { color: var(--yellow); border-color: var(--yellow); }
  .stage { color: var(--green); border-color: var(--green); }
  button.on { border-color: var(--accent); color: var(--accent); }
  button.armed, button.armed:hover { background: var(--red); color: #fff; border-color: var(--red); opacity: 1; }
</style>
