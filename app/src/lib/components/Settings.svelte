<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "#lib/api.ts";
  import { app, act, toast } from "#lib/state.svelte.ts";
  import { theme, setTheme } from "#lib/theme.svelte.ts";
  import { THEME_SETTINGS, isThemeSetting } from "#lib/themes.ts";

  let info = $state<{ path: string; home: string; text: string; effective: string } | null>(null);
  let text = $state("");

  async function load() {
    try {
      info = await api.configInfo();
      text = info.text || "";
    } catch (e) { toast(String(e)); }
  }
  onMount(load);

  const dirty = $derived(!!info && text !== (info.text || ""));

  async function save() {
    const ok = await act(async () => { await api.saveConfig(text); return true; }, "Saved. New agents and lookups use it immediately.");
    // The window's own settings (cost warning, the ticket workflow) too.
    if (ok) api.defaults().then((d) => { app.you = d.your_name; app.defaults = d; }).catch(() => {});
    // Show what the saved file now resolves to.
    if (ok) await load();
  }

  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") { e.preventDefault(); if (dirty) save(); }
  }
</script>

<div class="page scroll">
  <h2>Settings</h2>
  <div class="theme">
    <label for="theme-picker">Theme</label>
    <select id="theme-picker" value={theme.setting}
      onchange={(e) => isThemeSetting(e.currentTarget.value) && setTheme(e.currentTarget.value)}>
      {#each THEME_SETTINGS as t}<option value={t.id}>{t.name}</option>{/each}
    </select>
    <span class="muted">Also in View → Theme.</span>
  </div>
  {#if info}
    <p class="muted">Config: <code>{info.path}</code> · State and run logs: <code>{info.home}</code></p>
    <div class="cols">
      <div>
        <label for="cfg">Your config.toml (overrides the defaults)</label>
        <textarea id="cfg" rows="28" bind:value={text} spellcheck="false" onkeydown={onKey}></textarea>
        <div class="row" style="margin-top: 8px">
          <button class="primary" disabled={!dirty} onclick={save} title="Save (⌘S)">Save</button>
          {#if dirty}<button onclick={() => (text = info!.text || "")}>Revert</button>{/if}
          <span class="muted">{dirty ? "Unsaved changes. " : ""}Validated before saving.</span>
        </div>
      </div>
      <div>
        <label for="eff">Effective configuration</label>
        <pre id="eff" class="eff">{info.effective}</pre>
      </div>
    </div>
  {/if}
</div>

<style>
  .page { padding: 14px 20px; height: 100%; }
  h2 { margin: 0 0 6px; font-size: 18px; }
  .theme { display: flex; align-items: center; gap: 10px; margin: 6px 0 14px; }
  .theme label { margin: 0; }
  .theme select { width: 200px; }
  .cols { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
  .eff { background: var(--panel); border: 1px solid var(--border); border-radius: 6px; padding: 10px; max-height: 620px; overflow: auto; margin: 0; }
</style>
