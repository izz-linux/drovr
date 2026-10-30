<script lang="ts">
  import { api, ago, type ReportFile } from "$lib/api";
  import { app, act, toast, select } from "$lib/state.svelte";

  let dir = $state("");
  let files = $state<ReportFile[]>([]);
  let current = $state<ReportFile | null>(null);
  let query = $state("");
  let collapsed = $state<Record<string, boolean>>({});
  let expanded = $state<Record<string, boolean>>({});
  let feedback = $state("");
  let target = $state("");
  let frameKey = $state(0);

  async function load() {
    try {
      const r = await api.reports();
      dir = r.dir;
      files = r.files;
    } catch (e) { toast(String(e)); }
  }

  $effect(() => {
    load();
    const t = setInterval(load, 4000);
    return () => clearInterval(t);
  });

  // A report and the earlier copies of it: same folder, agent and title.
  type Report = { key: string; f: ReportFile; older: ReportFile[] };

  // Root first, then subfolders by most recent activity. Files come newest
  // first, so the first of each title is the latest version.
  const folders = $derived.by(() => {
    const q = query.toLowerCase();
    const map = new Map<string, Map<string, Report>>();
    for (const f of files) {
      if (q && !`${f.title} ${f.folder} ${f.agent} ${f.summary}`.toLowerCase().includes(q)) continue;
      if (!map.has(f.folder)) map.set(f.folder, new Map());
      const reports = map.get(f.folder)!;
      const key = `${f.folder}\n${f.agent}\n${f.title}`;
      const r = reports.get(key);
      if (r) r.older.push(f);
      else reports.set(key, { key, f, older: [] });
    }
    return [...map.entries()]
      .map(([folder, reports]) => [folder, [...reports.values()]] as const)
      .sort(([a, x], [b, y]) => (a === "" ? -1 : b === "" ? 1 : y[0].f.modified - x[0].f.modified));
  });
  const unseen = (r: Report) => !r.f.seen || r.older.some((f) => !f.seen);

  async function open(f: ReportFile) {
    current = f;
    target = f.agent;
    frameKey++;
    if (!f.seen) { await api.reportSeen(f.path); f.seen = true; }
  }

  async function markAllRead() {
    await act(() => api.reportsAllSeen());
    for (const f of files) f.seen = true;
  }

  async function sendFeedback() {
    if (!current || !target || !feedback.trim()) return;
    const msg = `Feedback from ${app.you} on "${current.title}" (${current.path}):\n\n${feedback}`;
    await act(() => api.prompt(target, msg), `Sent to ${target}`);
    feedback = "";
  }

  const folderPath = (folder: string) => (folder ? `${dir}/${folder}` : dir);
</script>

{#snippet item(f: ReportFile, old: boolean)}
  <button class="item" class:old class:sel={current?.path === f.path} onclick={() => open(f)}>
    <div class="row">
      {#if !f.seen}<span class="dot"></span>{/if}
      <strong class="grow t">{f.title}</strong>
      <span class="muted small">{ago(f.modified)}</span>
    </div>
    {#if f.agent && !old}<div class="muted small">{f.agent}</div>{/if}
    {#if f.summary}<div class="sum">{f.summary}</div>{/if}
  </button>
{/snippet}

<div class="wrap">
  <section class="side">
    <div class="head">
      <input placeholder="Filter reports" bind:value={query} />
      {#if files.some((f) => !f.seen)}
        <button class="ghost" title="Mark every report as read" onclick={markAllRead}>Mark all read</button>
      {/if}
      <button class="ghost" title="Show the plans folder in Finder" onclick={() => api.openPath(dir).catch((e) => toast(String(e)))}>⌂</button>
    </div>
    <div class="scroll grow">
      {#each folders as [folder, list] (folder)}
        <button class="folder" onclick={() => (collapsed[folder] = !collapsed[folder])}>
          <span class="caret">{collapsed[folder] ? "▸" : "▾"}</span>
          <span class="grow">{folder || "plans"}</span>
          {#if list.some(unseen)}<span class="dot"></span>{/if}
          <span class="muted small">{list.length}</span>
        </button>
        {#if !collapsed[folder]}
          {#each list as r (r.key)}
            {@render item(r.f, false)}
            {#if r.older.length}
              <button class="older" onclick={() => (expanded[r.key] = !expanded[r.key])}>
                <span class="caret">{expanded[r.key] ? "▾" : "▸"}</span>
                {r.older.length} earlier {r.older.length === 1 ? "version" : "versions"}
                {#if r.older.some((f) => !f.seen)}<span class="dot"></span>{/if}
              </button>
              {#if expanded[r.key]}
                {#each r.older as f (f.path)}{@render item(f, true)}{/each}
              {/if}
            {/if}
          {/each}
        {/if}
      {:else}
        <div class="empty">No HTML files in <code>{dir || "~/.claude/plans"}</code> yet. Agents save reports there with <code>publish_report</code>: a group's reports go in its own subfolder.</div>
      {/each}
    </div>
  </section>

  <section class="main">
    {#if current}
      <div class="row bar">
        <strong class="grow t" title={current.path}>{current.title}</strong>
        <button onclick={() => frameKey++}>Reload</button>
        <button onclick={() => api.openPath(current!.path).catch((e) => toast(String(e)))}>Open in browser</button>
        <button onclick={() => api.openPath(folderPath(current!.folder)).catch((e) => toast(String(e)))}>Show folder</button>
      </div>
      {#key frameKey}<iframe title={current.title} src={api.fileSrc(current.path)}></iframe>{/key}
      <form class="row bar foot" onsubmit={(e) => { e.preventDefault(); sendFeedback(); }}>
        <select bind:value={target} style="width: 220px">
          <option value="">Send feedback to…</option>
          {#each app.snap.agents as a}<option value={a.name}>{a.name}</option>{/each}
        </select>
        <input class="grow" bind:value={feedback} placeholder={target ? `Feedback for ${target}…` : "Pick an agent to reply to"} />
        <button class="primary" type="submit" disabled={!target || !feedback.trim()}>Send</button>
        {#if current.agent}<button type="button" class="ghost" onclick={() => select(current!.agent)}>View {current.agent}</button>{/if}
      </form>
    {:else}
      <div class="empty">Pick a report to review it.</div>
    {/if}
  </section>
</div>

<style>
  .wrap { display: grid; grid-template-columns: 340px 1fr; height: 100%; min-height: 0; }
  .side { border-right: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .head button { white-space: nowrap; }
  .head { display: flex; gap: 6px; padding: 10px; border-bottom: 1px solid var(--border); }
  .folder { display: flex; gap: 6px; align-items: center; width: 100%; text-align: left; background: var(--panel); border: none; border-bottom: 1px solid var(--border); border-radius: 0; padding: 7px 10px; font-weight: 600; }
  .caret { color: var(--muted); width: 10px; }
  .item { display: block; width: 100%; text-align: left; background: transparent; border: none; border-bottom: 1px solid var(--border); border-radius: 0; padding: 8px 12px 8px 26px; }
  .item.sel { background: var(--panel-2); }
  .item.old { padding-left: 40px; }
  .older { display: flex; gap: 6px; align-items: center; width: 100%; text-align: left; background: transparent; border: none; border-bottom: 1px solid var(--border); border-radius: 0; padding: 4px 12px 4px 26px; color: var(--muted); font-size: 11px; }
  .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }
  .small { font-size: 11px; }
  .sum { font-size: 12px; margin-top: 2px; }
  .main { display: flex; flex-direction: column; min-height: 0; min-width: 0; }
  .bar { padding: 8px 12px; border-bottom: 1px solid var(--border); }
  .foot { border-top: 1px solid var(--border); border-bottom: none; }
  iframe { flex: 1; border: 0; background: #fff; }
</style>
