<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api, type CliStatus, type LoginPathInfo, type RemoteInfo } from "#lib/api.ts";
  import { app, act, toast } from "#lib/state.svelte.ts";
  import { theme, setTheme } from "#lib/theme.svelte.ts";
  import { THEME_SETTINGS, isThemeSetting } from "#lib/themes.ts";

  let info = $state<{ path: string; home: string; text: string; effective: string } | null>(null);
  let text = $state("");

  let remote = $state<RemoteInfo | null>(null);
  let showQr = $state(false);

  let cli = $state<CliStatus | null>(null);
  let installing = $state(false);
  let pathInfo = $state<LoginPathInfo | null>(null);
  const noAnswer = (p: LoginPathInfo) => p.error?.includes("no answer") ?? false;
  const why = (p: LoginPathInfo, what: string) => (noAnswer(p) ? `${what} didn't answer` : `${what} failed`);
  const hint = (p: LoginPathInfo) =>
    noAnswer(p) ? ` Something in your shell's startup file may be waiting for input; run \`${p.shell} -l -i -c 'echo done'\` to see it.` : "";
  function pathLine(p: LoginPathInfo): string {
    const secs = (p.ms / 1000).toFixed(1);
    switch (p.source) {
      case "pending": return "PATH: asking your shell…";
      case "interactive": return `PATH: from your shell (${secs}s)`;
      case "login": return `PATH: from your login profile; ${why(p, "your interactive shell")}${noAnswer(p) ? " in 20s" : ""}${hint(p)}`;
      case "saved": return `PATH: saved from an earlier start; ${why(p, "your shell")}${hint(p)}`;
      case "system": return `PATH: macOS's default; ${why(p, "your shell")} and nothing was saved. Set full paths (watch.gh, opencode.command) or see USAGE.${hint(p)}`;
      case "kept": return "PATH: kept as launched (DROVR_KEEP_PATH)";
      default: return "";
    }
  }
  let pathTimer: ReturnType<typeof setTimeout> | undefined;
  function loadPath() {
    api.loginPathInfo().then((p) => {
      pathInfo = p;
      if (p.source === "pending") pathTimer = setTimeout(loadPath, 2000);
    }).catch(() => {});
  }
  const loadCli = () => api.cliStatus().then((c) => (cli = c)).catch(() => {});
  async function installCli() {
    installing = true;
    try {
      const msg = await act(() => api.installCli());
      if (msg) toast(msg);
    } finally {
      installing = false;
      loadCli();
    }
  }

  async function load() {
    loadCli();
    loadPath();
    api.remoteInfo().then((r) => (remote = r)).catch(() => {});
    try {
      info = await api.configInfo();
      text = info.text || "";
    } catch (e) { toast(String(e)); }
  }
  onMount(load);
  onDestroy(() => clearTimeout(pathTimer));

  const dirty = $derived(!!info && text !== (info.text || ""));

  async function save() {
    const ok = await act(async () => { await api.saveConfig(text); return true; }, "Saved. New agents and lookups use it immediately.");
    // The window's own settings (cost warning, the ticket workflow) too.
    if (ok) api.defaults().then((d) => { app.you = d.your_name; app.defaults = d; }).catch(() => {});
    // Show what the saved file now resolves to.
    if (ok) await load();
  }

  let cleaning = $state(false);
  async function cleanUp() {
    cleaning = true;
    try {
      const out = await act(() => api.cleanupNow());
      if (!out) return;
      const n = out.archived.length;
      const m = out.inbox_deleted;
      const bad = out.errors.length;
      toast(
        `Archived ${n} plan${n === 1 ? "" : "s"} and removed ${m} Inbox item${m === 1 ? "" : "s"}.` +
          (bad ? ` ${bad} problem${bad === 1 ? "" : "s"}: ${out.errors[0]}${bad > 1 ? " (and more)" : ""}` : ""),
      );
    } finally {
      cleaning = false;
    }
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
  <div class="phone">
    <label for="phone-link">Phone</label>
    {#if remote?.enabled}
      <div class="row">
        <input id="phone-link" readonly value={remote.link} />
        <button onclick={() => navigator.clipboard.writeText(remote!.link).then(() => toast("Copied. It signs a phone in, so keep it private."))}>Copy link</button>
        <button onclick={() => (showQr = !showQr)}>{showQr ? "Hide" : "Show"} QR code</button>
      </div>
      <p class="muted">
        {remote.status ? `The page is ${remote.status}.` : "The page starts when drovr does: restart drovr to start it."}
        Push notifications are {remote.push ? "on" : "off (set remote.ntfy_topic)"}.
      </p>
      <!-- The SVG is drovr's own QR code of the link above. -->
      {#if showQr}<div class="qr">{@html remote.qr_svg}</div>{/if}
    {:else}
      <p class="muted">To check in from your phone, set <code>[remote] enabled = true</code> below and restart drovr. README → Phone access has the rest.</p>
    {/if}
  </div>
  <div class="cli">
    <label for="cli-install">Command line tool</label>
    <div class="row">
      <button id="cli-install" onclick={installCli} disabled={installing}>{installing ? "Installing…" : cli?.installed ? "Reinstall" : "Install"}</button>
      <span class="muted">{cli?.installed ? `Installed at ${cli.target}.` : "Not installed."} Also in the drovr menu → Install Command Line Tool…</span>
    </div>
    {#if cli?.shadowed && cli.on_path}
      <p class="muted">Your shell runs <code>{cli.on_path}</code> first.</p>
    {/if}
  </div>
  <div class="cleanup">
    <div class="row">
      <button onclick={cleanUp} disabled={cleaning || dirty} title={dirty ? "Save first" : ""}>{cleaning ? "Cleaning up…" : "Clean up now"}</button>
    </div>
    <p class="muted">Archives plans whose newest file is older than <code>reports.archive_after_days</code> and removes resolved Inbox items older than <code>inbox.keep_days</code> (0 = off). Also runs daily.</p>
  </div>
  {#if pathInfo && pathLine(pathInfo)}
    <p class="muted" title={pathInfo.error ?? pathInfo.path}>{pathLine(pathInfo)}</p>
  {/if}
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
  .phone { margin: 0 0 14px; max-width: 900px; }
  .phone input { flex: 1; font-family: var(--mono); font-size: 12px; }
  .cli { margin: 0 0 14px; max-width: 900px; }
  .cleanup { margin: 0 0 14px; max-width: 900px; }
  .qr { background: #fff; padding: 12px; border-radius: 8px; width: fit-content; }
  .cols { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 16px; }
  .eff { background: var(--panel); border: 1px solid var(--border); border-radius: 6px; padding: 10px; max-height: 620px; overflow: auto; margin: 0; }
</style>
