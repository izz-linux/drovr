<script lang="ts">
  import { api } from "#lib/api.ts";
  import { app, jira } from "#lib/state.svelte.ts";
  import { md } from "#lib/md.ts";
  import LookupProgress from "./LookupProgress.svelte";


  async function search() {
    jira.loading = "Searching Jira…";
    jira.error = "";
    try {
      jira.results = await api.jiraSearch(jira.query);
      if (jira.results.length === 1) open(jira.results[0].key);
    } catch (e) { jira.error = String(e); }
    jira.loading = "";
  }

  async function open(key: string) {
    jira.loading = `Loading ${key}…`;
    jira.error = "";
    try { jira.ticket = await api.jiraGet(key); } catch (e) { jira.error = String(e); }
    jira.loading = "";
  }

  // An agent's ticket field can list several, e.g. "SRE-3230, SRE-3253".
  const owners = $derived(
    jira.ticket ? app.snap.agents.filter((a) => a.ticket.split(",").map((t) => t.trim()).includes(jira.ticket!.key)) : [],
  );
</script>

<div class="wrap">
  <section class="list">
    <form class="head" onsubmit={(e) => { e.preventDefault(); search(); }}>
      <input placeholder="SRE-123, JQL, or text (empty = your open tickets)" bind:value={jira.query} />
      <button class="primary" type="submit" disabled={!!jira.loading}>Search</button>
    </form>
    <div class="scroll grow">
      {#each jira.results as t}
        <button class="item" class:sel={jira.ticket?.key === t.key} onclick={() => open(t.key)}>
          <div class="row"><strong>{t.key}</strong><span class="grow"></span><span class="pill">{t.status}</span></div>
          <div class="summary">{t.summary}</div>
          <div class="muted small">{t.issue_type} · {t.priority} · {t.assignee}</div>
        </button>
      {:else}
        {#if jira.loading}<LookupProgress label={jira.loading} />{:else}<div class="empty">Search for tickets by key, JQL or text. Lookups go through your Jira MCP.</div>{/if}
      {/each}
    </div>
  </section>

  <section class="detail scroll">
    {#if jira.error}<div class="error-banner">{jira.error}</div>{/if}
    {#if jira.loading && jira.results.length}<LookupProgress label={jira.loading} />{/if}
    {#if jira.ticket}
      <div class="row">
        <h2 class="grow">{jira.ticket.key} · {jira.ticket.summary}</h2>
        {#if jira.ticket.url}<button onclick={() => api.openUrl(jira.ticket!.url)}>Open in Jira</button>{/if}
        <button class="primary" onclick={() => (app.dispatching = jira.ticket)}>Hand off to agents…</button>
      </div>
      <div class="meta">
        <span class="pill">{jira.ticket.issue_type}</span><span class="pill">{jira.ticket.status}</span>
        <span class="pill">{jira.ticket.priority}</span><span class="pill">{jira.ticket.assignee}</span>
      </div>
      {#if owners.length}
        <div class="owners">Being worked by:
          {#each owners as a}<button class="ghost" onclick={() => { app.selected = a.name; app.view = "agents"; }}>{a.name} <span class="pill s-{a.state}">{a.state}</span></button>{/each}
        </div>
      {/if}
      <h3>Description</h3>
      <div class="md">{@html md(jira.ticket.description || "_No description._")}</div>
      {#if jira.ticket.comments.length}
        <h3>Comments</h3>
        {#each jira.ticket.comments as c}<div class="comment md">{@html md(c)}</div>{/each}
      {/if}
    {:else if !jira.error}
      <div class="empty">Pick a ticket to read it, then hand it off to an agent, a new group, or a group you already have.</div>
    {/if}
  </section>
</div>

<style>
  .wrap { display: grid; grid-template-columns: 380px 1fr; height: 100%; min-height: 0; }
  .list { border-right: 1px solid var(--border); display: flex; flex-direction: column; min-height: 0; }
  .head { display: flex; gap: 6px; padding: 10px; border-bottom: 1px solid var(--border); }
  .item { display: block; width: 100%; text-align: left; background: transparent; border: none; border-bottom: 1px solid var(--border); border-radius: 0; padding: 8px 12px; }
  .item.sel { background: var(--panel-2); }
  .summary { margin: 2px 0; }
  .small { font-size: 11px; }
  .detail { padding: 14px 20px; }
  h2 { margin: 0; font-size: 17px; }
  h3 { margin: 18px 0 6px; font-size: 13px; color: var(--muted); text-transform: uppercase; letter-spacing: .05em; }
  .meta { display: flex; gap: 6px; margin-top: 8px; }
  .owners { margin-top: 10px; font-size: 12px; }
  .comment { border-left: 2px solid var(--border); padding-left: 10px; margin: 8px 0; }
</style>
