<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { api } from "$lib/api";

  // Shown while a Jira / Confluence / Slack lookup runs: what it's doing,
  // how long it's been, and a way to stop it.
  let { label }: { label: string } = $props();

  let step = $state("");
  let seconds = $state(0);

  onMount(() => {
    const started = Date.now();
    const timer = setInterval(() => (seconds = Math.floor((Date.now() - started) / 1000)), 500);
    const off = listen<string>("lookup-progress", (e) => (step = e.payload));
    return () => {
      clearInterval(timer);
      off.then((f) => f());
    };
  });
</script>

<div class="progress">
  <div class="row">
    <span class="spinner" aria-hidden="true"></span>
    <strong class="grow">{label}</strong>
    <span class="muted">{seconds}s</span>
    <button class="ghost" onclick={() => api.cancelLookups()}>Cancel</button>
  </div>
  <div class="muted step">{step || "Starting…"}</div>
  {#if seconds > 20}<div class="muted hint">Lookups go through your MCP servers with a one-shot agent, so they take a little while. The model is <code>agent_queries.model</code> in Settings.</div>{/if}
</div>

<style>
  .progress { padding: 16px; display: flex; flex-direction: column; gap: 6px; }
  .step { font-family: var(--mono); font-size: 12px; padding-left: 22px; }
  .hint { font-size: 12px; padding-left: 22px; }
  .spinner { width: 12px; height: 12px; border-radius: 50%; border: 2px solid var(--border); border-top-color: var(--accent); animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
