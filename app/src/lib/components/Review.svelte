<script lang="ts">
  import { api, type Repo } from "#lib/api.ts";
  import { app, act, toast } from "#lib/state.svelte.ts";

  let info = $state<{ cwd: string; repos: Repo[] } | null>(null);
  let root = $state("");
  let file = $state("");
  let diff = $state("");
  let mode = $state<"diff" | "preview">("diff");
  let error = $state("");
  let feedback = $state("");
  let frameKey = $state(0);
  let loading = $state(false);
  let closed = $state<Record<string, boolean>>({});

  const agent = $derived(app.snap.agents.find((a) => a.name === app.selected));
  const repo = $derived(info?.repos.find((r) => r.root === root));
  // Repos the agent only looked at start collapsed.
  const isClosed = (r: Repo) => closed[r.root] ?? r.changes.length === 0;

  async function load() {
    if (!app.selected) return;
    error = "";
    loading = true;
    try {
      info = await api.review(app.selected);
      const keep = info.repos.find((r) => r.root === root);
      if (keep) await show(keep.root, keep.changes.some((c) => c.path === file) ? file : "");
      else if (info.repos.length) await show(info.repos[0].root, "");
      else { root = ""; file = ""; diff = ""; }
    } catch (e) { error = String(e); info = null; }
    loading = false;
  }

  async function show(r: string, path: string) {
    root = r;
    file = path;
    if (mode === "preview" && !path.match(/\.html?$/i)) mode = "diff";
    const scope = info?.repos.find((x) => x.root === r)?.scope ?? [];
    try { diff = await api.diff(r, path, scope); } catch (e) { diff = String(e); }
  }

  function preview(r: string, path: string) {
    root = r;
    file = path;
    mode = "preview";
    frameKey++;
  }

  $effect(() => { app.selected; load(); });

  // Big diffs (a lockfile, generated code) have tens of thousands of lines:
  // only the ones in view, plus a margin, are in the page.
  const ROW = 18;
  let diffEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewH = $state(600);
  const diffLines = $derived(diff.split("\n"));
  const widest = $derived(diffLines.reduce((m, l) => Math.max(m, l.length + 7 * (l.split("\t").length - 1)), 0));
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 30));
  const last = $derived(Math.min(diffLines.length, Math.ceil((scrollTop + viewH) / ROW) + 30));
  $effect(() => {
    diff;
    if (diffEl) diffEl.scrollTop = scrollTop = 0;
  });

  function cls(line: string) {
    if (line.startsWith("+++") || line.startsWith("---")) return "meta";
    if (line.startsWith("+")) return "add";
    if (line.startsWith("-")) return "del";
    if (line.startsWith("@@")) return "hunk";
    if (line.startsWith("diff ")) return "file";
    return "";
  }

  async function sendFeedback() {
    if (!agent || !feedback.trim()) return;
    const where = file ? ` on ${repo ? `${repo.label}/` : ""}${file}` : repo ? ` on ${repo.label}` : "";
    await act(() => api.prompt(agent.name, `Review feedback from ${app.you}${where}:\n\n${feedback}`), "Feedback sent");
    feedback = "";
  }

  const full = (p: string) => `${root}/${p}`;
</script>

<div class="wrap">
  <section class="side">
    <div class="head">
      <select bind:value={app.selected}>
        {#each app.snap.agents as a}<option value={a.name}>{a.name}</option>{/each}
      </select>
      <button onclick={load} disabled={loading} title="Refresh">↻</button>
    </div>
    {#if info}
      <div class="scroll grow">
        {#each info.repos as r (r.root)}
          <div class="repo">
            <button class="rhead" onclick={() => (closed[r.root] = !isClosed(r))} title={r.root}>
              <span class="caret">{isClosed(r) ? "▸" : "▾"}</span> <strong>{r.label}</strong>
            </button>
            <div class="muted small pad">{r.summary}</div>
            {#if r.scope.length}<div class="muted small pad" title="This repo holds the plans folder, so it's every agent's: only what this agent worked in is shown">Only {r.scope.join(", ")}</div>{/if}
            {#if !isClosed(r)}
              <button class="f" class:sel={root === r.root && file === "" && mode === "diff"} onclick={() => show(r.root, "")}>
                <strong>All changes</strong> <span class="muted">({r.changes.length}{r.base !== "HEAD" ? ` since ${r.base}` : ""})</span>
              </button>
              {#each r.changes as c}
                <div class="frow">
                  <button class="f grow" class:sel={root === r.root && file === c.path && mode === "diff"} onclick={() => show(r.root, c.path)}
                    title={c.uncommitted ? "Not committed yet" : "Committed on this branch"}>
                    <code class="st" class:pending={c.uncommitted}>{c.status}</code> {c.path}
                  </button>
                  {#if c.is_html && c.status !== "D"}<button class="ghost" title="Preview" onclick={() => preview(r.root, c.path)}>👁</button>{/if}
                </div>
              {/each}
              {#if r.html.length}
                <div class="label">HTML pages</div>
                {#each r.html as h}
                  <div class="frow">
                    <button class="f grow" class:sel={root === r.root && file === h && mode === "preview"} onclick={() => preview(r.root, h)}>{h}</button>
                    <button class="ghost" title="Open in browser" onclick={() => api.openPath(`${r.root}/${h}`).catch((e) => toast(String(e)))}>↗</button>
                  </div>
                {/each}
              {/if}
            {/if}
          </div>
        {:else}
          <div class="empty small">No git repos yet. {agent?.name ?? "This agent"} started in {info.cwd}, which isn't a git repo, and hasn't edited files or run commands in one.</div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="main">
    {#if error}<div class="error-banner" style="padding: 8px 12px">{error}</div>{/if}
    {#if mode === "preview" && file && info}
      <div class="row bar"><code class="grow">{file}</code>
        <button onclick={() => frameKey++}>Reload</button>
        <button onclick={() => api.openPath(full(file)).catch((e) => toast(String(e)))}>Open in browser</button>
      </div>
      {#key frameKey}<iframe title={file} src={api.fileSrc(full(file))}></iframe>{/key}
    {:else}
      <div class="scroll grow diff" bind:this={diffEl} bind:clientHeight={viewH} onscroll={() => diffEl && (scrollTop = diffEl.scrollTop)}>
        {#if diff.trim()}
          <div class="rows" style="height: {diffLines.length * ROW}px; width: calc({widest}ch + 28px)">
            <div style="transform: translateY({first * ROW}px)">
              {#each diffLines.slice(first, last) as l, i (first + i)}<div class="l {cls(l)}">{l || " "}</div>{/each}
            </div>
          </div>
        {:else}<div class="empty">{!info ? "Select an agent." : repo ? `No changes in ${repo.label}${repo.base !== "HEAD" ? ` since ${repo.base}` : ""}.` : ""}</div>{/if}
      </div>
    {/if}
    {#if agent}
      <form class="row bar" onsubmit={(e) => { e.preventDefault(); sendFeedback(); }}>
        <input class="grow" bind:value={feedback} placeholder={`Feedback for ${agent.name}${file ? ` about ${file}` : ""}…`} />
        <button class="primary" type="submit" disabled={!feedback.trim()}>Send feedback</button>
      </form>
    {/if}
  </section>
</div>

<style>
  .wrap { display: grid; grid-template-columns: 320px 1fr; height: 100%; min-height: 0; }
  .side { border-right: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .head { display: flex; gap: 6px; padding: 10px; border-bottom: 1px solid var(--border); }
  .pad { padding: 2px 12px 6px; }
  .small { font-size: 11px; }
  .frow { display: flex; align-items: center; }
  .f { display: block; text-align: left; background: transparent; border: none; border-radius: 0; padding: 4px 12px; font-family: var(--mono); font-size: 12px;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; width: 100%; }
  .f.sel { background: var(--panel-2); }
  .st { color: var(--green); display: inline-block; min-width: 2ch; }
  .st.pending { color: var(--yellow); }
  .repo { border-bottom: 1px solid var(--border); padding-bottom: 6px; }
  .rhead { display: block; width: 100%; text-align: left; background: transparent; border: none; border-radius: 0; padding: 8px 12px 0; font-size: 13px;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .caret { color: var(--muted); }
  .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .05em; padding: 12px 12px 4px; }
  .main { display: flex; flex-direction: column; min-height: 0; min-width: 0; }
  .bar { padding: 8px 12px; border-bottom: 1px solid var(--border); }
  form.bar { border-top: 1px solid var(--border); border-bottom: none; }
  iframe { flex: 1; border: 0; background: #fff; }
  .diff { flex: 1; font-family: var(--mono); font-size: 12px; padding: 8px 0; min-height: 0; }
  .rows { min-width: 100%; }
  .l { white-space: pre; padding: 0 14px; height: 18px; line-height: 18px; overflow: hidden; }
  .add { background: color-mix(in srgb, var(--green) 14%, transparent); }
  .del { background: color-mix(in srgb, var(--red) 14%, transparent); }
  .hunk { color: var(--cyan); }
  .file { color: var(--purple); font-weight: 600; border-top: 1px solid var(--border); }
  .meta { color: var(--muted); }
</style>
