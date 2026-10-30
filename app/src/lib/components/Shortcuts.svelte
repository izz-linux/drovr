<script lang="ts">
  import { app } from "#lib/state.svelte.ts";
  import Modal from "./Modal.svelte";

  // ⌘ on macOS, Ctrl+ elsewhere.
  let { mod }: { mod: string } = $props();

  const groups: { title: string; keys: [string, string][] }[] = $derived([
    {
      title: "Anywhere",
      keys: [
        [`${mod}1 … ${mod}8`, "Agents, Inbox, Reports, Jira, Slack · Confluence, Message bus, Code review, Settings"],
        ...(app.defaults?.workflow ? ([[`${mod}9`, "Pipeline"]] as [string, string][]) : []),
        [`${mod}N`, "New agent"],
        [`${mod}G`, "New group"],
        [`${mod}F`, "Find in the agent's output, or filter the Message bus"],
        ["⌃`", "Show or hide the terminal"],
        [`${mod}/`, "This list"],
        ["Esc", "Close a dialog (twice if you've typed in it)"],
      ],
    },
    {
      title: "Agents",
      keys: [
        ["Enter", "Send the prompt"],
        ["Shift+Enter", "New line in the prompt"],
        ["Enter / Shift+Enter", "Next / previous match in Find"],
      ],
    },
    {
      title: "Inbox",
      keys: [
        ["↓ ↑ or J K", "Next / previous item"],
        ["1 … 9", "Pick that option of a decision, or a Watcher item's Send to"],
        ["A", "Allow a permission, or start a worker"],
        ["D", "Deny a permission (with the reason you typed), or decline a worker"],
      ],
    },
    { title: "Settings", keys: [[`${mod}S`, "Save your config"]] },
  ]);
</script>

<Modal title="Keyboard shortcuts" onclose={() => (app.shortcutsOpen = false)}>
  {#each groups as g}
    <h3>{g.title}</h3>
    <dl>
      {#each g.keys as [k, does]}<dt><kbd>{k}</kbd></dt><dd>{does}</dd>{/each}
    </dl>
  {/each}
  <p class="muted small">Ctrl works in place of ⌘, except in the terminal, where Ctrl keys go to the shell.</p>
</Modal>

<style>
  h3 { margin: 14px 0 6px; font-size: 12px; color: var(--muted); text-transform: uppercase; letter-spacing: .05em; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 6px 14px; margin: 0; }
  dt { white-space: nowrap; }
  dd { margin: 0; }
  kbd { font-family: var(--mono); font-size: 12px; background: var(--bg); border: 1px solid var(--border); border-radius: 4px; padding: 1px 6px; }
  .small { font-size: 12px; margin-top: 14px; }
</style>
