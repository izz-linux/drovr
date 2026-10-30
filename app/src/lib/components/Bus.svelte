<script lang="ts">
  import { api, ago, type Message, type Note } from "$lib/api";
  import { app, act } from "$lib/state.svelte";

  let group = $state("");
  let messages = $state<Message[]>([]);
  let notes = $state<Note[]>([]);
  let to = $state("@group");
  let body = $state("");

  $effect(() => {
    const g = group;
    let alive = true;
    const load = async () => {
      try {
        const b = await api.bus(g);
        if (alive) { messages = b.messages; notes = b.notes; }
      } catch {}
    };
    load();
    const t = setInterval(load, 2000);
    return () => { alive = false; clearInterval(t); };
  });

  const recipients = $derived(app.snap.agents.filter((a) => !group || a.group_name === group));

  async function send() {
    if (!body.trim()) return;
    const target = to === "@group" && !group ? "*" : to;
    const sent = await act(async () => { await api.send(target, group, body); return true; });
    if (sent) { body = ""; follow = true; }
  }

  // Newest messages are at the bottom: stay there unless you scroll up.
  let logEl: HTMLDivElement | undefined = $state();
  let follow = $state(true);
  $effect(() => {
    shown.length;
    if (follow && logEl) queueMicrotask(() => logEl && (logEl.scrollTop = logEl.scrollHeight));
  });

  // Messages are stored from/to "human"; show your name instead.
  const who = (name: string) => (name === "human" ? app.you : name);

  let filter = $state("");
  let filterEl: HTMLInputElement | undefined = $state();
  const shown = $derived(
    filter.trim()
      ? messages.filter((m) => `${who(m.sender)} ${who(m.recipient)} ${m.body}`.toLowerCase().includes(filter.trim().toLowerCase()))
      : messages,
  );
  let lastFind = app.find;
  $effect(() => {
    if (app.find === lastFind) return;
    lastFind = app.find;
    filterEl?.focus();
    filterEl?.select();
  });
</script>

<div class="wrap">
  <section class="main">
    <div class="row head">
      <select bind:value={group} style="width: 220px">
        <option value="">All messages</option>
        {#each app.snap.groups as g}<option value={g.name}>{g.name}</option>{/each}
      </select>
      <input class="filter" bind:this={filterEl} bind:value={filter} placeholder="Filter messages (⌘F)" onkeydown={(e) => e.key === "Escape" && (filter = "")} />
      <span class="muted grow">Messages you post here wake idle agents.</span>
    </div>
    <div class="scroll grow log" bind:this={logEl} onscroll={() => logEl && (follow = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 40)}>
      {#each shown as m}
        <div class="msg" class:human={m.sender === "human"}>
          <div class="row"><strong>{who(m.sender)}</strong><span class="muted">→ {who(m.recipient)}</span><span class="grow"></span><span class="muted small">{ago(m.ts)} ago</span></div>
          <div class="text">{m.body}</div>
        </div>
      {:else}<div class="empty">{filter.trim() ? "No messages match." : "No messages yet."}</div>{/each}
    </div>
    <form class="row compose" onsubmit={(e) => { e.preventDefault(); send(); }}>
      <select bind:value={to} style="width: 200px">
        <option value="@group">{group ? `@${group} (everyone)` : "* (all agents)"}</option>
        {#each recipients as a}<option value={a.name}>{a.name}</option>{/each}
      </select>
      <input class="grow" bind:value={body} placeholder={`Message as ${app.you}…`} />
      <button class="primary" type="submit">Send</button>
    </form>
  </section>
  <section class="notes scroll">
    <h3>Blackboard</h3>
    {#if !group}<div class="muted">Choose a group to see its shared notes.</div>{/if}
    {#each notes as n}
      <div class="note"><code>{n.key}</code><div>{n.value}</div><div class="muted small">{n.author} · {ago(n.ts)} ago</div></div>
    {:else}{#if group}<div class="muted">No notes yet.</div>{/if}{/each}
  </section>
</div>

<style>
  .wrap { display: grid; grid-template-columns: 1fr 320px; height: 100%; min-height: 0; }
  .main { display: flex; flex-direction: column; min-height: 0; border-right: 1px solid var(--border); }
  .filter { width: 240px; }
  .head { padding: 10px 14px; border-bottom: 1px solid var(--border); font-size: 12px; }
  .log { padding: 10px 14px; min-height: 0; }
  .msg { padding: 6px 10px; border-left: 2px solid var(--border); margin-bottom: 8px; }
  .msg.human { border-left-color: var(--purple); }
  .text { white-space: pre-wrap; }
  .small { font-size: 11px; }
  .compose { padding: 10px 14px; border-top: 1px solid var(--border); }
  .notes { padding: 10px 14px; }
  h3 { margin: 0 0 8px; font-size: 13px; color: var(--muted); text-transform: uppercase; letter-spacing: .05em; }
  .note { padding: 6px 0; border-bottom: 1px solid var(--border); }
</style>
