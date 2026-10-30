<script lang="ts">
  import type { Defaults } from "$lib/api";
  import { app } from "$lib/state.svelte";

  // What runs the agent (Claude Code or Codex) and where (headless or
  // herdr), preselected from the config.
  // `claudeOnly`: picking up an existing Claude Code session.
  let { defaults, backend = $bindable(), harness = $bindable(), id, claudeOnly = false }:
    { defaults: Defaults | null; backend: string; harness: string; id: string; claudeOnly?: boolean } = $props();

  $effect(() => {
    if (defaults && !backend) backend = defaults.backend;
    if (defaults && !harness) harness = defaults.harness || "claude";
  });
  // Codex agents run headless for now, and can't pick up Claude sessions.
  $effect(() => {
    if (harness === "codex" && claudeOnly) harness = "claude";
    if (harness === "codex" && backend === "herdr") backend = "headless";
  });
  const codex = $derived(harness === "codex");
</script>

<div class="two">
  <div>
    <label for={`${id}-harness`}>Run with</label>
    <select id={`${id}-harness`} bind:value={harness}>
      <option value="claude">Claude Code{defaults?.harness === "claude" ? " · default" : ""}</option>
      <option value="codex" disabled={!app.snap.codex || claudeOnly}>
        Codex (experimental){defaults?.harness === "codex" ? " · default" : ""}{claudeOnly ? " (not for Claude sessions)" : app.snap.codex ? "" : " (codex not found)"}
      </option>
    </select>
  </div>
  <div>
    <label for={id}>Run in</label>
    <select {id} bind:value={backend}>
      <option value="headless">Headless (managed by drovr){defaults?.backend === "headless" ? " · default" : ""}</option>
      <option value="herdr" disabled={!app.snap.herdr || codex}>
        herdr pane{defaults?.backend === "herdr" ? " · default" : ""}{codex ? " (Claude Code only)" : app.snap.herdr ? "" : " (herdr not found)"}
      </option>
    </select>
  </div>
</div>
{#if defaults}
  <p class="muted note">
    {#if codex}
      Codex runs in its own sandbox (<code>codex.sandbox</code>) with no permission prompts; anything outside it comes to your Inbox as a command to run.
    {:else}
      Model: <code>{defaults.model || "Claude Code default"}</code> ·
      Permission mode: <code>{defaults.permission_mode}</code> ·
      Worktrees: {defaults.worktrees ? "on" : "off"}
    {/if}
    <span class="hint">(change in Settings)</span>
  </p>
{/if}

<style>
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .note { font-size: 12px; margin: 6px 0 0; }
  .hint { opacity: 0.8; }
</style>
