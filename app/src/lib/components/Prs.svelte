<script lang="ts">
  import { api, ago, holdText, type Hold, type ReportFile, type ReviewPr } from "#lib/api.ts";
  import { app, act, select, toast } from "#lib/state.svelte.ts";

  // PRs waiting on your review, from the poll to the merge. New ones wait for
  // you to dispatch a reviewer; Reviewing ones have one.
  let busy = $state<Record<string, boolean>>({});
  let refreshing = $state(false);
  let reports = $state<ReportFile[]>([]);
  let hold = $state<Hold | null>(null);

  async function loadReports() {
    try { reports = (await api.reports()).files; } catch {}
  }
  async function loadHold() {
    try { hold = await api.prHold(); } catch {}
  }
  const load = () => { loadReports(); loadHold(); };
  $effect(() => {
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });

  const rows = $derived(app.snap.review_prs);
  const fresh = $derived(rows.filter((r) => r.status === "new"));
  const reviewing = $derived(rows.filter((r) => r.status === "dispatched"));

  const name = (r: ReviewPr) => `${r.owner}/${r.repo}#${r.number}`;
  const title = (r: ReviewPr) => r.title || name(r);
  const ci = (r: ReviewPr) => ({ success: "✓", failure: "✗", pending: "●" })[r.ci] ?? "";
  const ciText = (r: ReviewPr) => ({ success: "CI passing", failure: "CI failing", pending: "CI running" })[r.ci] ?? "";

  /** GitHub's decision, with your own latest review if you've left one. */
  function mine(r: ReviewPr): { text: string; tone: string } | null {
    const own = ({ APPROVED: "you approved", CHANGES_REQUESTED: "you requested changes", COMMENTED: "you commented" } as Record<string, string>)[r.my_review];
    if (own) return { text: own, tone: r.my_review === "APPROVED" ? "ok" : r.my_review === "CHANGES_REQUESTED" ? "warn" : "" };
    if (r.review_decision === "REVIEW_REQUIRED") return { text: "waiting on you", tone: "warn" };
    return null;
  }
  const decision = (r: ReviewPr) =>
    ({ APPROVED: "approved", CHANGES_REQUESTED: "changes requested" } as Record<string, string>)[r.review_decision] ?? "";

  // The report a reviewer published, if any: the newest titled "Review owner/repo#n".
  const norm = (t: string) => t.trim().toLowerCase();
  function reportFor(r: ReviewPr): ReportFile | undefined {
    const want = norm(`Review ${name(r)}`);
    return reports.filter((f) => norm(f.title) === want).sort((a, b) => b.modified - a.modified)[0];
  }
  const starting = (r: ReviewPr) => r.status === "dispatched" && !r.agent;
  const agentOf = (r: ReviewPr) => app.snap.agents.find((a) => a.name === r.agent);
  function reviewer(r: ReviewPr): string {
    const a = agentOf(r);
    if (!a) return "reviewer gone";
    if (a.state === "idle" && reportFor(r)) return "reported";
    return a.state;
  }

  async function once(key: string, fn: () => Promise<unknown>) {
    if (busy[key]) return;
    busy[key] = true;
    await act(fn);
    busy[key] = false;
  }
  const dispatch = (r: ReviewPr) => once(r.key, () => api.prDispatch(r.key));
  const dismiss = (r: ReviewPr) => once(r.key, () => api.prDismiss(r.key));

  function openReport(f: ReportFile) {
    app.openReport = f.title;
    app.view = "reports";
  }

  async function refreshNow() {
    if (refreshing) return;
    refreshing = true;
    try { await api.prRefresh(); } catch (e) { toast(String(e)); }
    await loadHold();
    await act(async () => {});
    refreshing = false;
  }
</script>

{#snippet card(r: ReviewPr)}
  {@const m = mine(r)}
  {@const rep = reportFor(r)}
  <article class="card" class:busy={busy[r.key]} aria-busy={busy[r.key] ? "true" : undefined}>
    <div class="row head">
      <strong class="name">{name(r)}</strong>
      {#if r.draft}<span class="pill">draft</span>{/if}
      <span class="grow"></span>
      <span class="muted small">{ago(r.offered_ts)} ago</span>
    </div>
    <div class="title">{title(r)}</div>
    <div class="row meta muted small">
      {#if r.author}<span>{r.author}</span>{/if}
      {#if r.source}<span>{r.source}</span>{/if}
      {#if ci(r)}<span class="ci c-{r.ci}" title={ciText(r)}>{ci(r)} {ciText(r)}</span>{/if}
      {#if decision(r)}<span class:ok={r.review_decision === "APPROVED"} class:warn={r.review_decision === "CHANGES_REQUESTED"}>{decision(r)}</span>{/if}
      {#if m}<span class:ok={m.tone === "ok"} class:warn={m.tone === "warn"}>{m.text}</span>{/if}
      {#if r.status === "dispatched" && !starting(r)}<span class="pill s-{agentOf(r)?.state ?? ''}">{reviewer(r)}</span>{/if}
    </div>
    <div class="row actions">
      {#if r.status === "new"}
        <button title="Start a reviewer agent on this PR" class="primary" onclick={() => dispatch(r)} disabled={busy[r.key]}>{busy[r.key] ? "Dispatching…" : "Dispatch reviewer"}</button>
      {:else if starting(r)}
        <span class="muted">Starting…</span>
      {:else}
        {#if agentOf(r)}<button title="Go to this PR's reviewer agent" onclick={() => select(r.agent)}>Open reviewer</button>{/if}
        {#if rep}<button title="Open the reviewer's report" onclick={() => openReport(rep)}>Report</button>{/if}
      {/if}
      <button title="Open this PR on GitHub in your browser" onclick={() => api.openUrl(r.url)}>Open on GitHub</button>
      <button class="ghost" onclick={() => dismiss(r)} disabled={busy[r.key]} title={r.status === "dispatched" ? "Removes the reviewer and hides the PR" : "Hide this PR"}>Dismiss</button>
    </div>
  </article>
{/snippet}

<div class="page scroll">
  <div class="row top">
    <h2 class="grow">PRs</h2>
    {#if holdText(hold)}<span class="muted small">{holdText(hold)}</span>{/if}
    <button title="Look for new PRs and refresh CI and reviews now" onclick={refreshNow} disabled={refreshing}>{refreshing ? "Refreshing…" : "Refresh"}</button>
  </div>

  {#if !rows.length}
    <div class="empty muted">No PRs waiting on you.</div>
  {/if}
  {#if fresh.length}
    <h3>New <span class="muted small">{fresh.length}</span></h3>
    {#each fresh as r (r.key)}{@render card(r)}{/each}
  {/if}
  {#if reviewing.length}
    <h3>Reviewing <span class="muted small">{reviewing.length}</span></h3>
    {#each reviewing as r (r.key)}{@render card(r)}{/each}
  {/if}
</div>

<style>
  .page { padding: 14px 20px; height: 100%; }
  .top { margin-bottom: 8px; }
  h2 { margin: 0; font-size: 18px; }
  h3 { margin: 14px 0 6px; font-size: 14px; }
  .small { font-size: 11px; }
  .card { background: var(--panel); border: 1px solid var(--border); border-left: 3px solid var(--accent); border-radius: 8px; padding: 10px 14px; margin-bottom: 10px; max-width: 980px; }
  .head { align-items: baseline; }
  .name { font-weight: 600; }
  .title { margin: 4px 0; }
  .meta { flex-wrap: wrap; gap: 4px 14px; }
  .actions { margin-top: 8px; flex-wrap: wrap; }
  .ci.c-success, .ok { color: var(--green); }
  .ci.c-failure { color: var(--red); }
  .warn { color: var(--yellow); }
  .ci.c-pending { color: var(--cyan); }
  .busy .actions button { opacity: .45; pointer-events: none; }
  .empty { padding: 24px 0; }
</style>
