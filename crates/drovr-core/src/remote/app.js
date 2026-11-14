// drovr's phone page: the Inbox, agents, reports, code review and the
// message bus, over the API in remote.rs. No build step and no libraries.
"use strict";

const $ = (id) => document.getElementById(id);
const main = $("main");

const state = {
  snap: null,
  signedOut: false,
  view: "inbox",
  agent: "",
  sub: "",
  showResolved: false,
  reportsKey: "",
};

// -- helpers -------------------------------------------------------------------

/** An element: h("div", { class: "x", onclick }, "text", child…). */
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (k === "value") el.value = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

/** What can go into an element: nodes, text and numbers, never false/null/undefined. */
const clean = (kids) => kids.flat().filter((k) => k instanceof Node || typeof k === "number" || (typeof k === "string" && k));
const fill = (el, ...kids) => el.replaceChildren(...clean(kids));
const put = (el, ...kids) => el.append(...clean(kids));

function ago(ts) {
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 60) return `${Math.floor(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** A little Markdown, escaped first, so only these tags can appear. */
function md(text) {
  const parts = String(text || "").split(/```[^\n]*\n?/);
  let html = "";
  parts.forEach((part, i) => {
    if (i % 2) { html += `<pre><code>${esc(part.replace(/\n$/, ""))}</code></pre>`; return; }
    const block = esc(part)
      .replace(/^#{1,6} (.*)$/gm, "<strong>$1</strong>")
      .replace(/`([^`\n]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
    html += block.split(/\n{2,}/).map((p) => p.trim() ? `<p>${p.replace(/\n/g, "<br>")}</p>` : "").join("");
  });
  const div = h("div", { class: "md" });
  div.innerHTML = html;
  return div;
}

let toastTimer;
function toast(text, err = false) {
  const t = $("toast");
  t.textContent = text;
  t.className = err ? "err" : "";
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), err ? 6000 : 2500);
}

async function call(path, body) {
  const opts = { headers: { "X-Drovr": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    opts.method = "POST";
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  let r;
  try {
    r = await fetch(`/api/${path}`, opts);
  } catch (e) {
    conn(false);
    throw new Error("Can't reach drovr. Is your Mac awake and on the same network?");
  }
  conn(true);
  const j = await r.json().catch(() => ({}));
  if (r.status === 401) { state.signedOut = true; render(); throw new Error(j.error || "signed out"); }
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}
const get = (path, params = {}) => call(`${path}?${new URLSearchParams(params)}`);

function conn(ok) {
  $("conn").className = `conn ${ok ? "ok" : "bad"}`;
}

/** Run an action from a card or button, then refresh. */
async function act(fn, ok, el) {
  el?.classList.add("busy");
  try {
    await fn();
    if (ok) toast(ok);
    await refresh();
  } catch (e) {
    toast(String(e.message || e), true);
  } finally {
    el?.classList.remove("busy");
  }
}

// -- routing -------------------------------------------------------------------

function route() {
  const hash = decodeURIComponent(location.hash.slice(1));
  const [view, ...rest] = hash.split("/");
  if (view.startsWith("item-")) {
    state.view = "inbox";
    state.flash = Number(view.slice(5));
  } else if (view === "agent") {
    state.view = "agent";
    state.agent = rest[0] || "";
    state.sub = rest[1] || "";
  } else {
    state.view = ["inbox", "agents", "reports", "bus"].includes(view) ? view : "inbox";
  }
  for (const a of document.querySelectorAll("nav#tabs a")) {
    a.classList.toggle("on", a.dataset.view === (state.view === "agent" ? "agents" : state.view));
  }
  main.replaceChildren();
  inboxCards.clear();
  closeReport();
  render();
  loadView();
}

const go = (hash) => (location.hash = hash);
const agentHash = (name, sub = "") => `#agent/${encodeURIComponent(name)}${sub ? `/${sub}` : ""}`;

// -- polling -------------------------------------------------------------------

async function refresh() {
  try {
    state.snap = await get("snapshot");
    state.signedOut = false;
  } catch {
    return;
  }
  const n = state.snap.inbox.length;
  $("badge").hidden = !n;
  $("badge").textContent = n;
  $("rbadge").hidden = !state.snap.unseen_reports;
  $("rbadge").textContent = state.snap.unseen_reports;
  document.title = n ? `(${n}) drovr` : "drovr";
  render();
}

/** Things only some views need, fetched when they open and while they're shown. */
async function loadView() {
  try {
    if (state.view === "agent" && state.sub === "") await loadOutput();
    else if (state.view === "agent" && state.sub === "review") await loadReview();
    else if (state.view === "reports") await loadReports();
    else if (state.view === "bus") await loadBus();
  } catch (e) {
    if (!state.signedOut) toast(String(e.message || e), true);
  }
}

setInterval(() => { if (!document.hidden) refresh(); }, 3000);
setInterval(() => {
  if (document.hidden) return;
  if (state.view === "agent" && state.sub === "") loadOutput().catch(() => {});
  if (state.view === "bus") loadBus().catch(() => {});
}, 3000);
document.addEventListener("visibilitychange", () => { if (!document.hidden) { refresh(); loadView(); } });
window.addEventListener("hashchange", route);

// -- rendering -----------------------------------------------------------------

function render() {
  if (state.signedOut) {
    $("where").textContent = "";
    fill(main, h("div", { class: "empty" },
      h("h2", {}, "Not signed in"),
      h("p", {}, "Open the link from ", h("code", {}, "drovr remote"), " or from drovr's Settings on your Mac (scan its QR code). This phone then stays signed in.")));
    return;
  }
  if (!state.snap) return;
  if (state.view === "inbox") renderInbox();
  else if (state.view === "agents") renderAgents();
  else if (state.view === "agent") renderAgent();
  else if (state.view === "reports") renderReports();
  else if (state.view === "bus") renderBus();
}

/** A section of `main` that's built once per view and found again by id. */
function section(id, build) {
  let el = main.querySelector(`[data-sec="${id}"]`);
  if (!el) {
    el = build();
    el.dataset.sec = id;
    main.append(el);
  }
  return el;
}

// -- inbox ---------------------------------------------------------------------

const kinds = {
  decision: "Decision", command: "Command", permission: "Permission", spawn: "Spawn", done: "Finished",
  watch: "Watcher", jira: "Jira draft", handoff: "Handoff", confluence: "Confluence",
};
/** Cards stay as built while their item waits, so what you type survives a refresh. */
const inboxCards = new Map();

function renderInbox() {
  $("where").textContent = "Inbox";
  const list = section("inbox", () => h("div"));
  const items = [...state.snap.inbox].sort((a, b) => a.id - b.id);
  const ids = new Set(items.map((i) => i.id));
  for (const [id, el] of inboxCards) if (!ids.has(id)) { el.remove(); inboxCards.delete(id); }
  let prev = null;
  for (const item of items) {
    let el = inboxCards.get(item.id);
    if (!el) { el = card(item); inboxCards.set(item.id, el); }
    if (prev ? prev.nextSibling !== el : list.firstChild !== el) prev ? prev.after(el) : list.prepend(el);
    prev = el;
  }
  let empty = list.querySelector(".empty");
  if (!items.length && !empty) list.append(h("div", { class: "empty" }, "Nothing needs you right now."));
  if (items.length && empty) empty.remove();

  if (state.flash && inboxCards.has(state.flash)) {
    const el = inboxCards.get(state.flash);
    el.scrollIntoView({ block: "start" });
    el.classList.add("flash");
    setTimeout(() => el.classList.remove("flash"), 2000);
    state.flash = 0;
  }

  const resolved = section("resolved", () => h("div", { style: "margin-top: 16px" }));
  fill(resolved, 
    h("button", { class: "ghost", onclick: () => { state.showResolved = !state.showResolved; render(); } },
      state.showResolved ? "Hide recently resolved" : "Show recently resolved"),
    ...(state.showResolved ? state.snap.recent.map((i) => h("div", { class: "msg small" },
      h("span", { class: "pill" }, i.kind), " ", h("strong", {}, i.agent), " ", i.title,
      h("div", { class: "muted" }, `→ ${i.status}${i.answer ? `: ${i.answer.split("\n")[0].slice(0, 120)}` : ""}`))) : []),
  );
}

function card(item) {
  const el = h("article", { class: `card k-${item.kind}`, id: `item-${item.id}` });
  const answer = (text, ok) => act(() => call("answer", { id: item.id, answer: text }), ok, el);
  const reject = (reason, ok) => act(() => call("reject", { id: item.id, reason }), ok, el);
  const dismiss = (ok) => act(() => call("dismiss", { id: item.id }), ok, el);
  const p = item.payload || {};
  const input = (placeholder, value = "") => h("input", { placeholder, value });

  put(el, h("div", { class: "row" },
    h("span", { class: "pill" }, kinds[item.kind] || item.kind),
    h("a", { href: agentHash(item.agent), class: "ellipsis" }, item.agent),
    h("span", { class: "muted small grow" }, `#${item.id} · ${ago(item.ts)} ago`),
    h("button", { class: "ghost", title: "Dismiss without answering", onclick: () => dismiss() }, "✕")));

  if (item.kind === "command") {
    const cmd = h("textarea", { rows: 3, value: p.command || "" });
    const note = input("Output or note for Mark as run, or reason to reject");
    put(el, md(item.body), cmd, h("div", { class: "muted small" }, `in ${p.cwd || "?"}`),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => confirm("Run this command on your Mac?") &&
          act(() => call("run", { id: item.id, target: "local", command: cmd.value }), "Command finished; output sent to the agent", el) }, "Run & return output"),
        state.snap.herdr && h("button", { onclick: () => act(() => call("run", { id: item.id, target: "herdr", command: cmd.value }), "Running in herdr", el) }, "Run in herdr"),
        note,
        h("button", { onclick: () => act(() => call("ran", { id: item.id, note: note.value }), "Marked as run; the agent was told", el) }, "Mark as run"),
        h("button", { class: "danger", onclick: () => reject(note.value) }, "Reject")));
  } else if (item.kind === "permission") {
    const why = input("Tell it why, or what to do instead (optional)");
    put(el, h("h3", {}, h("code", {}, item.title)), wrapBody(item.body),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => answer("allow", "Allowed") }, "Allow"),
        h("button", { class: "danger", onclick: () => answer(why.value ? `deny: ${why.value}` : "deny", "Denied") }, "Deny"),
        why));
  } else if (item.kind === "spawn") {
    const model = input(p.default_model || "the default model", p.model || "");
    const you = state.snap.you || "you";
    put(el, h("h3", {}, item.title), wrapBody(item.body),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => answer(model.value.trim() ? `approve model=${model.value.trim()}` : "approve", "Worker started") }, "Start worker"),
        h("button", { class: "danger", onclick: () => reject(`${you.charAt(0).toUpperCase()}${you.slice(1)} declined this worker.`) }, "Decline"),
        h("label", { class: "muted small", style: "flex-basis: 100%" }, "Model"), model));
  } else if (item.kind === "handoff") {
    const model = p.next ? input("the default model", p.next_model || "") : null;
    const notes = input("Notes for sending it back");
    put(el, h("h3", {}, item.title),
      h("details", { open: true }, h("summary", {}, p.file || "Handoff"), wrapBody(item.body)),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => {
          const m = model?.value.trim();
          answer(m ? `Approve model=${m}` : "Approve", p.next ? `Approved; the lead starts ${p.next}` : "Approved; the stages are done");
        } }, "Approve"),
        model && h("label", { class: "muted small", style: "flex-basis: 100%" }, `${p.next} runs on`), model,
        notes,
        h("button", { class: "danger", onclick: () => reject(notes.value, "Sent back with your notes") }, "Send back")));
  } else if (item.kind === "jira") {
    const comment = p.action === "comment";
    const text = comment ? h("textarea", { rows: 5, value: p.body || "" }) : input("Status", p.transition || "");
    put(el, h("h3", {}, item.title),
      comment ? text : h("div", {}, h("span", { class: "muted" }, `Move ${p.ticket} to`), text),
      p.fields && h("div", { class: "muted small" }, `with ${p.fields}`),
      h("div", { class: "muted small" }, "Nothing is posted until you do."),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => answer(text.value, comment ? "Comment posted to Jira" : "Ticket moved in Jira") }, "Post to Jira"),
        h("button", { onclick: () => dismiss("Discarded") }, "Discard")));
  } else if (item.kind === "confluence") {
    const notes = input("Notes for sending it back");
    const frame = h("iframe", { class: "preview", sandbox: "", title: `Preview of ${p.title || ""}` });
    frame.srcdoc = p.html || "";
    put(el, h("h3", {}, item.title), wrapBody(item.body),
      p.action === "update" && h("details", {}, h("summary", {}, "Changes from the live draft"), diffView(p.diff || "")),
      h("details", {}, h("summary", {}, "The full page"), frame),
      h("div", { class: "actions" },
        h("button", { class: "primary", onclick: () => answer("approve", "Saved as a draft in Confluence") }, "Save as draft"),
        notes,
        h("button", { class: "danger", onclick: () => reject(notes.value, "Sent back with your notes") }, "Send back")));
  } else if (item.kind === "done") {
    put(el, h("h3", {}, item.title), wrapBody(item.body),
      h("div", { class: "actions" },
        h("button", { onclick: () => go(agentHash(item.agent, "review")) }, "Review changes"),
        h("button", { onclick: () => dismiss() }, "Acknowledge")));
  } else {
    const text = h("textarea", { rows: 2, placeholder: "Or write an answer…" });
    put(el, h("h3", {}, item.title), item.body && wrapBody(item.body),
      item.options?.length && h("div", { class: "options" },
        item.options.map((opt, i) => h("button", { onclick: () => answer(opt, "Answer sent") }, h("span", { class: "muted" }, i + 1), opt))),
      h("div", { class: "actions" }, text,
        h("button", { class: "primary", onclick: () => text.value.trim() && answer(text.value, "Answer sent") }, "Answer")));
  }
  return el;
}

const wrapBody = (body) => { const m = md(body); m.classList.add("body-scroll"); return m; };

function diffView(text) {
  return h("pre", { class: "diff" }, text.split("\n").map((l) =>
    h("div", { class: l.startsWith("+") ? "add" : l.startsWith("-") ? "del" : l.startsWith("@@") ? "hunk" : "" }, l || " ")));
}

// -- agents --------------------------------------------------------------------

function renderAgents() {
  $("where").textContent = "Agents";
  const agents = state.snap.agents;
  const list = section("agents", () => h("div"));
  if (!agents.length) { fill(list, h("div", { class: "empty" }, "No agents yet.")); return; }
  const groups = new Map();
  for (const a of agents) {
    const g = a.group_name || "";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(a);
  }
  const rows = [];
  for (const [g, list] of [...groups].sort(([a], [b]) => (a === "") - (b === "") || a.localeCompare(b))) {
    rows.push(h("div", { class: "group-name" }, g || "Solo"));
    for (const a of list.sort((x, y) => (y.role === "orchestrator") - (x.role === "orchestrator") || x.name.localeCompare(y.name))) {
      rows.push(agentRow(a));
    }
  }
  fill(list, ...rows);
}

function cost(a) {
  const warn = state.snap.cost_warning_usd > 0 && a.cost_usd > state.snap.cost_warning_usd;
  return a.cost_usd ? h("span", { class: `small cost${warn ? " warn" : ""}` }, `$${a.cost_usd.toFixed(2)}`) : null;
}

function agentRow(a) {
  return h("a", { class: `agent-row s-${a.state}`, href: agentHash(a.name) },
    h("span", { class: "dot" }),
    h("div", { class: "grow" },
      h("div", { class: "row" }, h("strong", { class: "grow ellipsis" }, a.name),
        a.pending ? h("span", { class: "badge" }, a.pending) : null, cost(a)),
      h("div", { class: "muted small" }, [a.state, a.role !== "solo" && a.role, a.stage, a.ticket].filter(Boolean).join(" · ")),
      a.status && h("div", { class: "small ellipsis" }, a.status)));
}

function renderAgent() {
  const a = state.snap.agents.find((x) => x.name === state.agent);
  $("where").textContent = state.agent;
  if (!a) { fill(main, h("div", { class: "empty" }, "No agent by that name."), h("a", { href: "#agents" }, "‹ Agents")); return; }
  const head = section("head", () => h("div"));
  const running = a.state === "working";
  fill(head, 
    h("div", { class: "row" },
      h("a", { href: "#agents" }, "‹ Agents"), h("span", { class: "grow" }),
      state.sub === "review" ? h("a", { href: agentHash(a.name) }, "Output") : h("a", { href: agentHash(a.name, "review") }, "Code review")),
    h("div", { class: `row s-${a.state}`, style: "margin: 10px 0 4px" }, h("span", { class: "dot", style: "margin-top: 0" }),
      h("strong", { class: "grow ellipsis" }, a.name), cost(a)),
    h("div", { class: "muted small" }, [a.state, a.role, a.harness, a.model, a.ticket].filter(Boolean).join(" · ")),
    a.status && h("div", { class: "small" }, a.status),
    a.pending ? h("div", { class: "small" }, h("a", { href: "#inbox" }, `${a.pending} waiting in your Inbox`)) : null,
    h("div", { class: "actions" },
      running && h("button", { class: "danger", onclick: () => act(() => call("stop", { name: a.name }), "Stopped") }, "Stop"),
      ["stopped", "error", "interrupted", "done", "idle"].includes(a.state) &&
        h("button", { onclick: () => act(() => call("resume", { name: a.name, text: "" }), "Resumed") }, "Resume")));
  if (state.sub === "review") { section("review", () => h("div", { style: "margin-top: 12px" })); return; }

  section("output", () => h("div", { class: "output", style: "margin-top: 12px" }, h("div", { class: "muted" }, "Loading…")));
  const queued = section("queued", () => h("div"));
  fill(queued, ...(a.queued || []).map((m) => h("div", { class: "queued" },
    h("div", { class: "muted small" }, "Queued"), h("div", {}, m.body),
    h("div", { class: "actions" },
      h("button", { onclick: () => act(() => call("send_now", { name: a.name, id: m.id }), "Sent; its run was cut short") }, "Send now"),
      h("button", { onclick: () => act(async () => {
        if (await call("unqueue", { name: a.name, id: m.id })) {
          const box = main.querySelector(".composer textarea");
          if (box) box.value = m.body + (box.value ? `\n${box.value}` : "");
        }
      }) }, "✕ Take back")))));
  section("composer", () => {
    const box = h("textarea", { rows: 3, placeholder: `Prompt ${a.name}…` });
    const send = () => box.value.trim() && act(async () => {
      const r = await call("prompt", { name: state.agent, text: box.value });
      box.value = "";
      if (r) toast(r);
    });
    return h("div", { class: "composer" }, box, h("div", { class: "actions" },
      h("button", { class: "primary", onclick: send }, "Send"),
      h("span", { class: "muted small grow" }, "Queued if it's busy.")));
  });
}

async function loadOutput() {
  const name = state.agent;
  const lines = await get("output", { name, lines: 300 });
  if (state.view !== "agent" || state.agent !== name || state.sub !== "") return;
  const box = main.querySelector('[data-sec="output"]');
  if (!box) return;
  const atEnd = box.scrollHeight - box.scrollTop - box.clientHeight < 40 || !box.dataset.loaded;
  fill(box, ...(lines.length ? lines.map((l) => h("div", { class: `l l-${l.kind}` }, l.text)) : [h("div", { class: "muted" }, "No output yet.")]));
  box.dataset.loaded = "1";
  if (atEnd) box.scrollTop = box.scrollHeight;
}

async function loadReview() {
  const name = state.agent;
  const box = () => main.querySelector('[data-sec="review"]');
  if (box()) fill(box(), h("div", { class: "muted" }, "Reading the repos…"));
  const info = await get("review", { name });
  if (state.agent !== name || state.sub !== "review" || !box()) return;
  if (!info.repos.length) { fill(box(), h("div", { class: "empty" }, "No changes found in its repos.")); return; }
  fill(box(), ...info.repos.map((repo) => {
    const out = h("div");
    const show = (path) => act(async () => {
      fill(out, h("div", { class: "muted" }, "Loading diff…"));
      const text = await get("diff", { name, root: repo.root, path });
      fill(out, h("div", { class: "row" }, h("strong", { class: "grow ellipsis mono" }, path || "All changes"),
        h("button", { class: "ghost", onclick: () => out.replaceChildren() }, "✕")), diffView(text || "No differences."));
      out.scrollIntoView({ block: "start" });
    });
    return h("section", { class: "card" },
      h("h3", {}, repo.label), h("div", { class: "muted small" }, repo.summary),
      h("div", { class: "muted small" }, `against ${repo.base}`),
      h("div", { class: "actions" }, h("button", { onclick: () => show("") }, "Whole diff")),
      ...repo.changes.map((c) => h("button", { class: "file-row", onclick: () => show(c.path) },
        h("span", { class: "st" }, c.status), h("span", { class: "grow mono small", style: "word-break: break-all" }, c.path),
        c.uncommitted ? h("span", { class: "muted small" }, "uncommitted") : null)),
      out);
  }));
}

// -- reports -------------------------------------------------------------------

function renderReports() {
  $("where").textContent = "Reports";
  section("reports", () => h("div", {}, h("div", { class: "muted" }, "Loading…")));
}

async function loadReports() {
  const r = await get("reports");
  state.reportsKey = r.key;
  const box = main.querySelector('[data-sec="reports"]');
  if (!box) return;
  if (!r.files.length) { fill(box, h("div", { class: "empty" }, "No reports yet.")); return; }
  const files = [...r.files].sort((a, b) => b.modified - a.modified);
  fill(box, ...files.map((f) => h("a", { class: `report-row${f.seen ? "" : " unseen"}`, href: "#reports", onclick: (e) => { e.preventDefault(); openReport(f); } },
    h("div", { class: "t" }, h("strong", {}, f.title || f.rel)),
    h("div", { class: "muted small" }, [f.folder, f.agent, `${ago(f.modified)} ago`].filter(Boolean).join(" · ")),
    f.summary && h("div", { class: "small" }, f.summary))));
}

function openReport(f) {
  const src = `/plans/${state.reportsKey}/${f.rel.split("/").map(encodeURIComponent).join("/")}`;
  $("report-title").textContent = f.title || f.rel;
  $("report-frame").src = src;
  $("report-open").href = src;
  $("report").hidden = false;
  if (!f.seen) call("report_seen", { path: f.path }).then(refresh).catch(() => {});
}
function closeReport() {
  if ($("report").hidden) return;
  $("report").hidden = true;
  $("report-frame").src = "about:blank";
  if (state.view === "reports") loadReports().catch(() => {});
}
$("report-close").addEventListener("click", closeReport);

// -- message bus ---------------------------------------------------------------

let busGroup = "";

function renderBus() {
  $("where").textContent = "Message bus";
  const head = section("bushead", () => {
    const pick = h("select", { onchange: (e) => { busGroup = e.target.value; loadBus(); } });
    return h("div", {}, pick);
  });
  const pick = head.querySelector("select");
  const groups = state.snap.groups.map((g) => g.name);
  const want = ["", ...groups].join("\n");
  if (pick.dataset.groups !== want) {
    fill(pick, h("option", { value: "" }, "All groups"), ...groups.map((g) => h("option", { value: g }, g)));
    pick.value = busGroup;
    pick.dataset.groups = want;
  }
  section("bus", () => h("div", { style: "margin-top: 8px" }));
  section("busform", () => {
    const to = h("input", { placeholder: "To: an agent, @group or *" });
    const body = h("textarea", { rows: 2, placeholder: "Message" });
    return h("div", { class: "composer" }, to, h("div", { style: "height: 6px" }), body,
      h("div", { class: "actions" }, h("button", { class: "primary", onclick: () => {
        if (!to.value.trim() || !body.value.trim()) return;
        let target = to.value.trim();
        let group = busGroup;
        if (target.startsWith("@")) { group = target.slice(1); target = `@${group}`; }
        act(async () => { await call("message", { to: target, group, body: body.value }); body.value = ""; await loadBus(); }, "Sent");
      } }, "Send")));
  });
}

/** How drovr stores you ("human"), as your name. */
const who = (name) => (name === "human" ? state.snap?.you || "you" : name);

async function loadBus() {
  const group = busGroup;
  const r = await get("bus", { group });
  const box = main.querySelector('[data-sec="bus"]');
  if (!box || group !== busGroup) return;
  const notes = r.notes.length ? [h("details", {}, h("summary", {}, `Shared notes (${r.notes.length})`),
    ...r.notes.map((n) => h("div", { class: "msg small" }, h("strong", {}, n.key), ` (${n.author}): `, n.value)))] : [];
  fill(box, ...notes, ...(r.messages.length ? [...r.messages].reverse().map((m) => h("div", { class: "msg" },
    h("div", { class: "muted small" }, `${who(m.sender)} → ${who(m.recipient)} · ${ago(m.ts)} ago`), md(m.body))) : [h("div", { class: "empty" }, "No messages.")]));
}

// -- start ---------------------------------------------------------------------

refresh().then(route);
