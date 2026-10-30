<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "#lib/api.ts";
  import { listen } from "@tauri-apps/api/event";
  import { app, refresh, type View } from "#lib/state.svelte.ts";
  import { initTheme } from "#lib/theme.svelte.ts";
  import Agents from "#lib/components/Agents.svelte";
  import Inbox from "#lib/components/Inbox.svelte";
  import Jira from "#lib/components/Jira.svelte";
  import Search from "#lib/components/Search.svelte";
  import Bus from "#lib/components/Bus.svelte";
  import Review from "#lib/components/Review.svelte";
  import Settings from "#lib/components/Settings.svelte";
  import Reports from "#lib/components/Reports.svelte";
  import NewAgent from "#lib/components/NewAgent.svelte";
  import NewGroup from "#lib/components/NewGroup.svelte";
  import Dispatch from "#lib/components/Dispatch.svelte";
  import Shortcuts from "#lib/components/Shortcuts.svelte";
  import Timeline from "#lib/components/Timeline.svelte";
  import Pipeline from "#lib/components/Pipeline.svelte";

  // Pipeline is part of the ticket workflow (workflow.enabled).
  const nav: { id: View; label: string; key: string }[] = $derived([
    { id: "agents", label: "Agents", key: "1" },
    { id: "inbox", label: "Inbox", key: "2" },
    { id: "reports", label: "Reports", key: "3" },
    { id: "jira", label: "Jira", key: "4" },
    { id: "search", label: "Slack · Confluence", key: "5" },
    { id: "bus", label: "Message bus", key: "6" },
    { id: "review", label: "Code review", key: "7" },
    { id: "settings", label: "Settings", key: "8" },
    ...(app.defaults?.workflow ? [{ id: "pipeline" as View, label: "Pipeline", key: "9" }] : []),
  ]);

  // Both ⌘ and Ctrl work; show the one people press on this platform.
  const mod = /Mac/i.test(navigator.platform || navigator.userAgent) ? "⌘" : "Ctrl+";

  const working = $derived(app.snap.agents.filter((a) => a.state === "working").length);
  const blocked = $derived(app.snap.agents.filter((a) => a.state === "blocked").length);

  onMount(() => {
    const stopTheme = initTheme();
    const offFind = listen("menu:menu-edit-find", () => app.find++);
    const offTerm = listen("menu:menu-view-terminal", () => {
      if (app.view !== "agents") { app.view = "agents"; app.termOpen = true; }
      else app.termOpen = !app.termOpen;
    });
    refresh();
    api.defaults().then((d) => { app.you = d.your_name; app.defaults = d; }).catch(() => {});
    const offKeys = listen("menu:menu-view-shortcuts", () => (app.shortcutsOpen = true));
    const t = setInterval(refresh, 2000);
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      // In the terminal, Ctrl keys belong to the shell (^N, ^G, …); ⌘ still works.
      if (!e.metaKey && (e.target as HTMLElement | null)?.closest?.(".xterm")) return;
      const item = nav.find((n) => n.key === e.key);
      if (item) { app.view = item.id; e.preventDefault(); }
      if (e.key === "n") { app.newAgentOpen = true; e.preventDefault(); }
      if (e.key === "g") { app.newGroupOpen = true; e.preventDefault(); }
      if (e.key === "/") { app.shortcutsOpen = true; e.preventDefault(); }
    };
    window.addEventListener("keydown", onKey);
    return () => { stopTheme(); offFind.then((f) => f()); offTerm.then((f) => f()); offKeys.then((f) => f()); clearInterval(t); window.removeEventListener("keydown", onKey); };
  });
</script>

<div class="shell">
  <aside>
    <div class="brand"><img src="/drovr.svg" alt="" width="22" height="22" />drovr</div>
    <nav>
      {#each nav as n}
        <button class="nav" class:active={app.view === n.id} aria-current={app.view === n.id ? "page" : undefined} onclick={() => (app.view = n.id)} title={`${mod}${n.key}`}>
          <span>{n.label}</span>
          {#if n.id === "inbox" && app.snap.inbox.length}<span class="badge">{app.snap.inbox.length}</span>{/if}
          {#if n.id === "reports" && app.snap.unseen_reports}<span class="badge new">{app.snap.unseen_reports}</span>{/if}
          {#if n.id === "agents" && app.snap.agents.length}<span class="count">{app.snap.agents.length}</span>{/if}
        </button>
      {/each}
    </nav>
    <div class="actions">
      <button class="primary" onclick={() => (app.newAgentOpen = true)}>New agent <span class="kbd">{mod}N</span></button>
      <button onclick={() => (app.newGroupOpen = true)}>New group <span class="kbd">{mod}G</span></button>
    </div>
    <div class="foot muted">
      <div><span class="dot cyan"></span>{working} working</div>
      <div><span class="dot yellow"></span>{blocked} need you</div>
      <div><span class="dot" class:green={app.snap.herdr}></span>herdr {app.snap.herdr ? "connected" : "not found"}</div>
      <button class="ghost keys" onclick={() => (app.shortcutsOpen = true)} title={`Keyboard shortcuts (${mod}/)`}>Keyboard shortcuts <span class="kbd">{mod}/</span></button>
    </div>
  </aside>

  <main>
    {#if app.error}<div class="error-banner banner">{app.error}</div>{/if}
    {#if app.view === "agents"}<Agents />
    {:else if app.view === "inbox"}<Inbox />
    {:else if app.view === "reports"}<Reports />
    {:else if app.view === "jira"}<Jira />
    {:else if app.view === "search"}<Search />
    {:else if app.view === "bus"}<Bus />
    {:else if app.view === "review"}<Review />
    {:else if app.view === "pipeline"}<Pipeline />
    {:else}<Settings />{/if}
  </main>
</div>

{#if app.newAgentOpen}<NewAgent />{:else if app.newGroupOpen}<NewGroup />{/if}
{#if app.dispatching}<Dispatch />{/if}
{#if app.shortcutsOpen}<Shortcuts {mod} />{/if}
{#if app.timeline}<Timeline />{/if}
<div class="toast-region" role="status" aria-live="polite">{#if app.toast}<div class="toast">{app.toast}</div>{/if}</div>

<style>
  .shell { display: grid; grid-template-columns: 210px 1fr; height: 100vh; }
  aside { background: var(--panel); border-right: 1px solid var(--border); display: flex; flex-direction: column; padding: 12px 10px; gap: 14px; }
  .brand { display: flex; align-items: center; gap: 8px; font-weight: 700; letter-spacing: .08em; font-size: 15px; padding: 2px 6px; }
  nav { display: flex; flex-direction: column; gap: 2px; }
  .nav { display: flex; justify-content: space-between; align-items: center; background: transparent; border-color: transparent; text-align: left; padding: 6px 8px; }
  .nav.active { background: var(--panel-2); border-color: var(--border); }
  .badge { background: var(--yellow); color: var(--bg); border-radius: 9px; font-size: 11px; padding: 0 7px; font-weight: 600; }
  .badge.new { background: var(--accent); color: var(--on-accent); }
  .count { color: var(--muted); font-size: 11px; }
  .actions { display: flex; flex-direction: column; gap: 6px; }
  .kbd { opacity: .6; font-size: 11px; margin-left: 4px; }
  .foot { margin-top: auto; font-size: 12px; display: flex; flex-direction: column; gap: 3px; padding: 0 6px; }
  .keys { text-align: left; padding: 2px 0; margin-top: 6px; font-size: 12px; color: var(--muted); }
  .dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: var(--muted); margin-right: 7px; }
  .dot.cyan { background: var(--cyan); } .dot.yellow { background: var(--yellow); } .dot.green { background: var(--green); }
  main { min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .banner { padding: 8px 14px; border-bottom: 1px solid var(--border); }
  .toast { position: fixed; bottom: 16px; right: 16px; max-width: 520px; background: var(--panel-2); border: 1px solid var(--border);
    border-radius: 8px; padding: 10px 14px; white-space: pre-wrap; box-shadow: 0 6px 24px rgba(0,0,0,.35); z-index: 50; }
</style>
