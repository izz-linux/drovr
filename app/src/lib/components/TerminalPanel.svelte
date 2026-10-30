<script lang="ts">
  import "@xterm/xterm/css/xterm.css";
  import type { Agent } from "#lib/api.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import { untrack } from "svelte";
  import { terminalFor, termKey, termLabel, typeInto, fitAll, stopJob, restartShell, attach } from "#lib/terminal.svelte.ts";

  let { agent }: { agent: Agent } = $props();
  let host: HTMLDivElement | undefined = $state();
  let height = $state(load());

  function load(): number {
    try { return Math.max(120, Number(localStorage.getItem("drovr.termHeight")) || 260); } catch { return 260; }
  }

  // Show this agent's terminal (its group's, or its own). Only when that
  // terminal changes: `agent` is a new object on every 2-second refresh, and
  // re-attaching then took focus from whatever you were typing in and reset
  // the scroll position.
  const key = $derived(termKey(agent));
  $effect(() => {
    key;
    const box = host;
    if (!box) return;
    const a = untrack(() => agent);
    let gone = false;
    terminalFor(a).then((t) => {
      if (gone) return;
      attach(t, box);
      // Never take focus from an open dialog.
      requestAnimationFrame(() => { if (!document.querySelector('[role="dialog"]')) t.term.focus(); });
    });
    return () => { gone = true; };
  });

  // A command sent here from the Inbox (Run in terminal).
  $effect(() => {
    const run = app.termRun;
    if (!run || run.agent !== agent.name) return;
    app.termRun = null;
    typeInto(agent, run.text, (m) => (app.termMarks[run.itemId] = m), () => toast("Waiting for your shell to start…"))
      .then(() => toast("Check the command, then press Enter. Mark as run in the Inbox sends its output to the agent."))
      .catch((e) => toast(String(e)));
  });

  $effect(() => {
    if (!host) return;
    const ro = new ResizeObserver(() => fitAll());
    ro.observe(host);
    return () => ro.disconnect();
  });

  function drag(e: PointerEvent) {
    const startY = e.clientY;
    const startH = height;
    const move = (m: PointerEvent) => (height = Math.min(window.innerHeight - 200, Math.max(120, startH + startY - m.clientY)));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      try { localStorage.setItem("drovr.termHeight", String(Math.round(height))); } catch {}
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<div class="panel" style="height: {height}px">
  <div class="grip" role="separator" aria-orientation="horizontal" onpointerdown={drag} title="Drag to resize"></div>
  <div class="bar muted">
    <span class="grow">{termLabel(agent)}</span>
    <button class="ghost" title="Force-stop what's running, when ^C doesn't work. The shell stays."
      onclick={() => stopJob(agent).then((stopped) => toast(stopped ? "Stopped the running command." : "Nothing is running in this terminal.")).catch((e) => toast(String(e)))}>Stop job</button>
    <button class="ghost" title="End this shell and everything running in it, and start a new one"
      onclick={() => restartShell(agent).catch((e) => toast(String(e)))}>Restart shell</button>
    <span class="hint">⌃` to hide</span>
    <button class="ghost" title="Hide (⌃`)" onclick={() => (app.termOpen = false)}>✕</button>
  </div>
  <div class="host" bind:this={host}></div>
</div>

<style>
  .panel { display: flex; flex-direction: column; border-top: 1px solid var(--border); background: var(--bg); min-height: 120px; flex: none; position: relative; }
  .grip { position: absolute; top: -3px; left: 0; right: 0; height: 6px; cursor: row-resize; z-index: 2; }
  .bar { display: flex; align-items: center; gap: 8px; padding: 2px 8px 2px 16px; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; }
  .bar button { padding: 0 6px; }
  .hint { text-transform: none; letter-spacing: 0; }
  .host { flex: 1; min-height: 0; padding: 0 0 4px 12px; }
  .host :global(.term-host) { height: 100%; }
  .host :global(.xterm) { height: 100%; }
</style>
