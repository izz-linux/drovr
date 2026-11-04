<script lang="ts">
  import { api, type PushPreview } from "#lib/api.ts";
  import { md } from "#lib/md.ts";
  import DiffLines from "./DiffLines.svelte";

  let { command, cwd, namedCwd }: { command: string; cwd: string; namedCwd: boolean } = $props();

  type Tab = "push" | "branch" | "pr";

  let open = $state(false);
  let loading = $state(false);
  let preview = $state<PushPreview | null>(null);
  let error = $state("");
  let loaded = $state<string | null>(null);
  let moved = $state(false);
  let tab = $state<Tab>("push");
  let seq = 0;

  const summaryOnly = $derived(/\bgh\s+pr\s+edit\b/.test(command) && !/\bgit\b[^\n;&|]*\bpush\b/.test(command));
  const stale = $derived(open && !loading && loaded !== null && loaded !== command);
  const tabs = $derived.by(() => {
    const p = preview;
    if (!p || p.unavailable) return [] as Tab[];
    const out: Tab[] = [];
    if (p.pushes) out.push("push");
    if (p.whole_branch) out.push("branch");
    if (p.pr_summary) out.push("pr");
    return out;
  });
  const shown = $derived(tabs.includes(tab) ? tab : tabs[0]);
  const names: Record<Tab, string> = { push: "This push", branch: "Whole branch", pr: "PR summary" };
  const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

  async function load(fresh: boolean) {
    const mine = ++seq;
    const cmd = command;
    loading = true;
    error = "";
    try {
      const p = await api.pushPreview(cmd, cwd, namedCwd);
      if (mine !== seq) return;
      preview = p;
      loaded = cmd;
      moved = false;
      if (fresh) tab = p.pushes ? "push" : "pr";
      loading = false;
      if (p.pushes && !p.unavailable) {
        api.pushGithubMoved(p.root, p.remote_branch, p.tracking)
          .then((m) => { if (mine === seq && m === true) moved = true; })
          .catch(() => {});
      }
    } catch (e) {
      if (mine !== seq) return;
      preview = null;
      loaded = cmd;
      error = String(e);
      loading = false;
    }
  }

  function toggle() {
    if (open) {
      open = false;
      seq++;
      loading = false;
      return;
    }
    open = true;
    preview = null;
    loaded = null;
    load(true);
  }
</script>

<div class="pp">
  <button class="view" aria-expanded={open} onclick={toggle}>
    {open ? "Hide" : "View"} {summaryOnly ? "PR summary" : "push"}
  </button>
  {#if open}
    <div class="body">
      <div class="row" class:notice={stale}>
        <span class="grow">{stale ? "Command changed" : ""}</span>
        <button disabled={loading} onclick={() => load(false)}>Refresh</button>
      </div>
      {#if loading && !preview}
        <div class="muted small">Loading…</div>
      {:else if error}
        <div class="notice">Couldn't preview this: {error}</div>
      {:else if preview?.unavailable}
        <div class="notice">{preview.unavailable}</div>
      {:else if preview}
        {#if preview.summary_line}<div class="sum">{preview.summary_line}</div>{/if}
        {#if preview.assumed_folder}
          <div class="notice">Assumed folder: the command doesn't give a full path, so this starts from the agent's working folder, which may not be where it runs.</div>
        {/if}
        {#each preview.notices as n}<div class="notice">{n}</div>{/each}
        {#if moved}<div class="notice warn">GitHub has moved since this repo last fetched; this diff may be out of date.</div>{/if}
        {#if tabs.length > 1}
          <div class="tabs" role="tablist">
            {#each tabs as t}
              <button role="tab" class="tab" class:sel={shown === t} aria-selected={shown === t} onclick={() => (tab = t)}>{names[t]}</button>
            {/each}
          </div>
        {/if}
        {#if shown === "push"}
          {#if preview.commits.length}
            <ul class="commits">{#each preview.commits as c}<li>{c}</li>{/each}</ul>
          {/if}
          {#if preview.this_push.trim()}
            <DiffLines text={preview.this_push} class="pp-diff" />
          {:else if preview.this_push_note}
            <div class="muted small">{preview.this_push_note}</div>
          {/if}
          {#if preview.uncommitted}
            <div class="label">Not committed yet; the final commit may differ.</div>
            <DiffLines text={preview.uncommitted} class="pp-diff" />
          {:else if preview.uncommitted_note}
            <div class="muted small">{preview.uncommitted_note}</div>
          {/if}
        {:else if shown === "branch" && preview.whole_branch}
          {@const w = preview.whole_branch}
          <div class="sum">{preview.branch} against {w.base} · {plural(w.commits, "commit")}, {plural(w.files, "file")}</div>
          <div class="muted small">Added lines from this push are highlighted; removed lines aren't marked.</div>
          {#if preview.uncommitted}<div class="muted small">Uncommitted changes aren't included; see This push.</div>{/if}
          <DiffLines text={w.text} marked={w.marked} class="pp-diff" />
        {:else if shown === "pr" && preview.pr_summary}
          {#if preview.pr_summary.text != null}
            <div class="md prose">{@html md(preview.pr_summary.text)}</div>
          {:else}
            <div class="muted small">{preview.pr_summary.note}</div>
          {/if}
        {:else}
          <div class="muted small">Nothing to show.</div>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .pp { margin: 6px 0; }
  .body { margin-top: 6px; border: 1px solid var(--border); border-radius: 6px; padding: 8px; background: var(--bg); }
  .small { font-size: 11px; margin-top: 2px; }
  .sum { font-size: 12px; font-weight: 600; margin: 2px 0 4px; }
  .notice { font-size: 12px; margin: 4px 0; padding: 4px 8px; border-left: 3px solid var(--yellow); background: var(--panel-2); border-radius: 3px; }
  .notice.warn { border-left-color: var(--red); }
  .tabs { display: flex; gap: 4px; margin: 6px 0; border-bottom: 1px solid var(--border); }
  .tab { background: transparent; border: none; border-bottom: 2px solid transparent; border-radius: 0; padding: 4px 10px; color: var(--muted); }
  .tab.sel { color: var(--text); border-bottom-color: var(--accent); }
  .commits { list-style: none; margin: 4px 0; padding: 0; font-family: var(--mono); font-size: 12px; }
  .label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .05em; padding: 10px 0 4px; }
  .prose { max-height: 420px; overflow: auto; }
  .body :global(.pp-diff) { max-height: 420px; border: 1px solid var(--border); border-radius: 4px; margin-top: 4px; }
</style>
