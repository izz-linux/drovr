<script lang="ts">
  import { api, type Page } from "$lib/api";
  import { app, act, toast, search as s, runSearch } from "$lib/state.svelte";
  import { md } from "$lib/md";
  import LookupProgress from "./LookupProgress.svelte";

  async function openPage(p: Page) {
    s.loading = true;
    try { s.page = await api.confluenceGet(p.id); } catch (e) { s.error = String(e); }
    s.loading = false;
  }

  /** Send a snippet of context to an agent as a message from you. */
  async function share(text: string) {
    if (!s.target) { toast("Pick an agent to send this to"); return; }
    await act(() => api.send(s.target, "", text), `Sent to ${s.target}`);
  }
</script>

<div class="page">
  <form class="bar" onsubmit={(e) => { e.preventDefault(); runSearch(); }}>
    <div class="tabs">
      <button type="button" class:on={s.source === "slack"} onclick={() => (s.source = "slack")}>Slack</button>
      <button type="button" class:on={s.source === "confluence"} onclick={() => (s.source = "confluence")}>Confluence</button>
      <button type="button" class:on={s.source === "ask"} onclick={() => (s.source = "ask")}>Ask (all MCPs)</button>
    </div>
    <input class="grow" bind:value={s.query}
      placeholder={s.source === "ask" ? "e.g. What did #payments decide about refunds last week? Link the Confluence spec." : "Search…"} />
    <button class="primary" type="submit" disabled={s.loading}>{s.loading ? "Working…" : s.source === "ask" ? "Ask" : "Search"}</button>
  </form>
  <div class="row share muted">
    Send results to
    <select bind:value={s.target} style="width: 220px">
      <option value="">choose an agent…</option>
      {#each app.snap.agents as a}<option value={a.name}>{a.name}</option>{/each}
    </select>
    <span>{s.target ? `Results go to ${s.target} when the search finishes, even if you switch views.` : "Pick an agent to send it the results automatically."}</span>
  </div>
  {#if s.error}<div class="error-banner pad">{s.error}</div>{/if}

  <div class="scroll body">
    {#if s.loading}<LookupProgress label={s.source === "ask" ? "Asking your MCPs…" : `Searching ${s.source === "slack" ? "Slack" : "Confluence"}…`} />{/if}
    {#if s.source === "slack"}
      {#each s.slack as m}
        <div class="card">
          <div class="row"><strong>#{m.channel}</strong><span class="muted">{m.user}</span><span class="grow"></span>
            {#if m.permalink}<button class="ghost" onclick={() => api.openUrl(m.permalink)}>open</button>{/if}
            <button class="ghost" onclick={() => share(`From Slack #${m.channel} (${m.user}): ${m.text}\n${m.permalink}`)}>send to agent</button>
          </div>
          <div class="md">{@html md(m.text)}</div>
        </div>
      {:else}<div class="empty">Search Slack. With the agent provider this goes through your Slack MCP.</div>{/each}
    {:else if s.source === "confluence"}
      <div class="split">
        <div>
          {#each s.pages as p}
            <button class="item" class:sel={s.page?.id === p.id} onclick={() => openPage(p)}>
              <strong>{p.title}</strong><div class="muted small">{p.space}</div><div class="small">{p.excerpt}</div>
            </button>
          {:else}<div class="empty">Search Confluence.</div>{/each}
        </div>
        <div class="pagebody">
          {#if s.page}
            <div class="row"><h2 class="grow">{s.page.title}</h2>
              {#if s.page.url}<button onclick={() => api.openUrl(s.page!.url)}>Open</button>{/if}
              <button onclick={() => share(`Confluence page "${s.page!.title}" ${s.page!.url}\n\n${s.page!.body.slice(0, 6000)}`)}>Send to agent</button>
            </div>
            <div class="md">{@html md(s.page.body)}</div>
          {/if}
        </div>
      </div>
    {:else if s.answer}
      <div class="card">
        <div class="row"><span class="grow muted">Answer</span><button class="ghost" onclick={() => share(s.answer)}>send to agent</button></div>
        <div class="md">{@html md(s.answer)}</div>
      </div>
    {:else}
      <div class="empty">Ask a question. A one-shot Claude agent answers it with every MCP you have (Slack, Jira, Confluence, …).</div>
    {/if}
  </div>
</div>

<style>
  .page { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .bar { display: flex; gap: 8px; padding: 10px 14px; border-bottom: 1px solid var(--border); align-items: center; }
  .tabs { display: flex; gap: 2px; }
  .tabs button.on { border-color: var(--accent); color: var(--accent); }
  .share { padding: 6px 14px; font-size: 12px; border-bottom: 1px solid var(--border); }
  .pad { padding: 8px 14px; }
  .body { flex: 1; padding: 12px 14px; min-height: 0; }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 8px 12px; margin-bottom: 8px; max-width: 980px; }
  .split { display: grid; grid-template-columns: 340px 1fr; gap: 14px; }
  .item { display: block; width: 100%; text-align: left; margin-bottom: 4px; }
  .item.sel { border-color: var(--accent); }
  .small { font-size: 12px; }
  .pagebody h2 { margin: 0; font-size: 16px; }
</style>
