<script lang="ts">
  import type { Defaults } from "#lib/api.ts";
  import { app } from "#lib/state.svelte.ts";

  // What runs the agent (Claude Code, OpenCode or Codex) and where
  // (headless or herdr), preselected from the config.
  // `adopting`: picking up an existing session, which Codex can't. `model`
  // is empty for the configured default.
  let { defaults, backend = $bindable(), harness = $bindable(), model = $bindable(""), modelLabel = "Model", id, adopting = false }:
    { defaults: Defaults | null; backend: string; harness: string; model?: string; modelLabel?: string; id: string; adopting?: boolean } = $props();

  $effect(() => {
    if (defaults && !backend) backend = defaults.backend;
    if (defaults && !harness) harness = defaults.harness || "claude";
  });
  // Codex agents run headless only, and can't pick up a session.
  $effect(() => {
    if (harness === "codex" && adopting) harness = "claude";
    if (headlessOnly && backend === "herdr") backend = "headless";
  });
  const codex = $derived(harness === "codex");
  const opencode = $derived(harness === "opencode");
  const headlessOnly = $derived(codex);
  const label = $derived(codex ? "Codex" : opencode ? "OpenCode" : "Claude Code");
  const defaultModel = $derived((codex ? defaults?.codex_model : opencode ? defaults?.opencode_model : defaults?.model) || `${label} default`);
</script>

<div class="two">
  <div>
    <label for={`${id}-harness`}>Run with</label>
    <select id={`${id}-harness`} bind:value={harness}>
      <option value="claude">Claude Code{defaults?.harness === "claude" ? " · default" : ""}</option>
      <option value="opencode" disabled={!app.snap.opencode}>
        OpenCode{defaults?.harness === "opencode" ? " · default" : ""}{app.snap.opencode ? "" : " (opencode not found)"}
      </option>
      <option value="codex" disabled={!app.snap.codex || adopting}>
        Codex (experimental){defaults?.harness === "codex" ? " · default" : ""}{adopting ? " (can't pick up a session)" : app.snap.codex ? "" : " (codex not found)"}
      </option>
    </select>
  </div>
  <div>
    <label for={id}>Run in</label>
    <select {id} bind:value={backend}>
      <option value="headless">Headless (managed by drovr){defaults?.backend === "headless" ? " · default" : ""}</option>
      <option value="herdr" disabled={!app.snap.herdr || headlessOnly}>
        herdr pane{defaults?.backend === "herdr" ? " · default" : ""}{headlessOnly ? " (not for Codex)" : app.snap.herdr ? "" : " (herdr not found)"}
      </option>
    </select>
  </div>
</div>
<div class="model">
  <label for={`${id}-model`}>{modelLabel}</label>
  <input id={`${id}-model`} bind:value={model} placeholder={defaultModel} spellcheck="false" autocomplete="off" />
</div>
{#if defaults}
  <p class="muted note">
    {#if codex}
      Codex runs in its own sandbox (<code>codex.sandbox</code>) with no permission prompts; anything outside it comes to your Inbox as a command to run.
    {:else if opencode}
      {#if backend === "herdr"}You answer OpenCode's permission prompts in its pane{:else}Permission mode: <code>{defaults.permission_mode}</code>, asked through drovr's OpenCode plugin{/if} ·
      Worktrees: {defaults.worktrees ? "on" : "off"}
    {:else}
      Permission mode: <code>{defaults.permission_mode}</code> ·
      Worktrees: {defaults.worktrees ? "on" : "off"}
    {/if}
    <span class="hint">(change these in Settings; leave {modelLabel} empty for <code>{defaultModel}</code>)</span>
  </p>
{/if}

<style>
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .model { margin-top: 10px; }
  .note { font-size: 12px; margin: 6px 0 0; }
  .hint { opacity: 0.8; }
</style>
