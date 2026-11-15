// drovr's phone page: the Inbox, agents, reports, code review and the
// Pipeline, over the API in remote.rs. No build step and no libraries.
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
    state.view = ["inbox", "agents", "reports", "pipeline"].includes(view) ? view : "inbox";
  }
  for (const a of document.querySelectorAll("nav#tabs a")) {
    a.classList.toggle("on", a.dataset.view === (state.view === "agent" ? "agents" : state.view));
  }
  main.replaceChildren();
  inboxCards.clear();
  closeReport();
  for (const sheet of document.querySelectorAll(".sheet")) sheet.remove();
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
  $("pipeline-tab").hidden = !state.snap.workflow;
  render();
}

/** Things only some views need, fetched when they open and while they're shown. */
async function loadView() {
  try {
    if (state.view === "agent" && state.sub === "") await loadOutput();
    else if (state.view === "agent" && state.sub === "review") await loadReview();
    else if (state.view === "reports") await loadReports();
    else if (state.view === "pipeline") await loadPipeline();
  } catch (e) {
    if (!state.signedOut) toast(String(e.message || e), true);
  }
}

setInterval(() => { if (!document.hidden) refresh(); }, 3000);
setInterval(() => {
  if (document.hidden) return;
  if (state.view === "agent" && state.sub === "") loadOutput().catch(() => {});
  if (state.view === "pipeline") loadPipeline().catch(() => {});
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
  else if (state.view === "pipeline") renderPipeline();
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

// -- pipeline ------------------------------------------------------------------

// Your sprint as the watcher sees it (Jira through your CLI, PRs through gh),
// next to the agents working each ticket: the app's Pipeline, as cards.
let pipe = null;
let showDone = false;
let reading = false;

/** Ticket keys in some text, upper-cased: "sre-3311 and SRE-2998" → both. */
const ticketsIn = (text) => [...new Set((String(text || "").match(/(?<![A-Za-z0-9-])[A-Za-z][A-Za-z0-9]+-\d+(?![A-Za-z0-9-])/g) || []).map((k) => k.toUpperCase()))];

/** An agent's tickets: its own, else its group's. */
function agentTickets(a) {
  const own = ticketsIn(a.ticket);
  if (own.length) return own;
  return ticketsIn(state.snap.groups.find((g) => g.name === a.group_name)?.ticket);
}

function renderPipeline() {
  $("where").textContent = "Pipeline";
  const box = section("pipeline", () => h("div", {}, h("div", { class: "muted" }, "Loading…")));
  if (!pipe) return;
  if (!pipe.workflow) {
    fill(box, h("div", { class: "empty" }, "The ticket workflow is off. Turn on ", h("code", {}, "workflow.enabled"), " in drovr's config to follow your sprint here."));
    return;
  }
  const ws = pipe.watch;
  const tickets = Object.values(ws.tickets || {}).sort((a, b) => String(b.updated).localeCompare(String(a.updated)));
  const prs = Object.values(ws.prs || {});
  const open = tickets.filter((t) => t.category !== "done");
  const done = tickets.filter((t) => t.category === "done");
  const known = new Set(tickets.map((t) => t.key));
  // Tickets agents are on that aren't in the sprint the watcher follows.
  const others = [...new Set(state.snap.agents.flatMap(agentTickets))].filter((k) => !known.has(k)).sort();

  const since = (c) => (c.at ? `${ago(c.at)} ago` : "not yet");
  const head = h("div", { class: "pipe-head" },
    h("div", { class: "row" },
      h("h2", { class: "grow", style: "margin: 0" }, "Your sprint"),
      h("button", { disabled: reading, onclick: refreshPipeline }, reading ? "Reading…" : "Refresh")),
    h("div", { class: "muted small" }, `${open.length} open · ${done.length} done · Jira read ${since(ws.jira)} · GitHub read ${since(ws.github)}`),
    ws.jira.error && h("div", { class: "err small" }, `Jira: ${ws.jira.error}`),
    ws.github.error && h("div", { class: "err small" }, `GitHub: ${ws.github.error}`));

  if (!ws.jira.at && !tickets.length) {
    fill(box, head, h("div", { class: "empty" }, "The watcher hasn't read Jira yet. Tap Refresh to read it now."));
    return;
  }
  fill(box, head,
    open.map((t) => ticketCard(t.key, t, prs)),
    others.map((k) => ticketCard(k, null, prs)),
    done.length && h("button", { class: "ghost", style: "margin-top: 4px", onclick: () => { showDone = !showDone; renderPipeline(); } },
      `${showDone ? "▾" : "▸"} Done (${done.length})`),
    showDone && done.map((t) => ticketCard(t.key, t, prs)));
}

function ticketCard(key, t, allPrs) {
  const agents = state.snap.agents.filter((a) => agentTickets(a).includes(key));
  const prs = allPrs.filter((p) => (p.tickets || []).includes(key));
  const waiting = state.snap.inbox.filter((i) => i.kind !== "done" && agents.some((a) => a.name === i.agent)).length;
  const cost = agents.reduce((s, a) => s + (a.cost_usd || 0), 0);
  const tone = !t ? "none" : t.category === "done" ? "done" : t.category === "indeterminate" ? "doing" : "todo";
  const failing = prs.flatMap((p) => (p.state === "OPEN" ? p.failing || [] : []).map((name, i) =>
    h("div", { class: "err small" }, `#${p.number} failing: `,
      p.failing_urls?.[i] ? h("a", { href: p.failing_urls[i], target: "_blank", rel: "noopener" }, name) : name)));
  return h("article", { class: `card ticket j-${tone}` },
    h("div", { class: "row" },
      t?.url ? h("a", { href: t.url, target: "_blank", rel: "noopener", class: "tkey" }, key) : h("strong", { class: "tkey" }, key),
      t ? h("span", { class: `pill j-${tone}` }, t.status) : h("span", { class: "muted small" }, "not in the sprint"),
      h("span", { class: "grow" }),
      waiting ? h("a", { href: "#inbox", class: "badge", title: "Waiting in your Inbox" }, `⚑ ${waiting}`) : null,
      agents.length ? h("span", { class: "small mono" }, `$${cost.toFixed(2)}`) : null),
    t?.summary && h("div", { class: "tsum" }, t.summary),
    t?.last_comment && h("div", { class: "muted small ellipsis" }, `💬 ${t.last_comment}`),
    agents.length
      ? h("div", { class: "chips" }, agents.map((a) => h("a", { class: `chip s-${a.state}`, href: agentHash(a.name) },
          h("span", { class: "dot" }), `${a.role === "orchestrator" ? "★ " : ""}${a.name}${a.stage ? ` · ${a.stage}` : ""}`)))
      : h("div", { class: "row", style: "margin-top: 8px" }, h("span", { class: "muted small grow" }, "No agents"),
          t && t.category !== "done" ? h("button", { onclick: () => openDispatch(t) }, "Dispatch…") : null),
    prs.length ? h("div", { class: "chips" }, prs.map(prChip)) : null,
    failing);
}

function prChip(p) {
  const bits = [];
  if (p.state === "MERGED") bits.push(h("span", { class: "ok" }, "merged"));
  else if (p.state === "CLOSED") bits.push(h("span", { class: "muted" }, "closed"));
  else {
    const ci = { success: "✓", failure: "✗", pending: "●" }[p.ci];
    if (ci) bits.push(h("span", { class: `ci c-${p.ci}` }, ci));
    if (p.draft) bits.push(h("span", { class: "muted" }, "draft"));
    const review = { APPROVED: "approved", CHANGES_REQUESTED: "changes requested", REVIEW_REQUIRED: "needs review" }[p.review];
    if (review) bits.push(h("span", { class: p.review === "APPROVED" ? "ok" : p.review === "CHANGES_REQUESTED" ? "warn" : "muted" }, review));
    if (p.mergeable === "CONFLICTING") bits.push(h("span", { class: "warn" }, "conflict"));
  }
  return h("a", { class: "chip pr", href: p.url, target: "_blank", rel: "noopener", title: `${p.repo}#${p.number} ${p.title}` },
    h("strong", {}, `#${p.number}`), ...bits);
}

async function loadPipeline() {
  pipe = await get("pipeline");
  if (state.view === "pipeline" && !reading) renderPipeline();
}

/** Read Jira and GitHub now, as the app's Refresh now does. */
async function refreshPipeline() {
  reading = true;
  renderPipeline();
  try {
    const watch = await call("watch_now", {});
    pipe = { ...pipe, watch };
  } catch (e) {
    toast(String(e.message || e), true);
  } finally {
    reading = false;
    renderPipeline();
  }
}

// -- dispatch ------------------------------------------------------------------

/** A name as drovr keeps them: lowercase letters, digits, - and _. */
const typedName = (v) => v.toLowerCase().replace(/\s+/g, "-").replace(/[^a-z0-9_-]/g, "");

/** Hand a ticket off, as the app's Dispatch dialog does. */
function openDispatch(t) {
  const d = pipe?.defaults || {};
  const groups = state.snap.groups.map((g) => g.name);
  // With the ticket workflow, a ticket gets its own group and lead, as in the app.
  const f = {
    mode: state.snap.workflow ? "group" : groups.length ? "existing" : "solo",
    name: t.key.toLowerCase(), cwd: d.cwd || "", harness: d.harness || "claude", backend: d.backend || "headless",
    model: "", notes: "", group: groups[0] || "", target: "",
  };
  let busy = false;
  const sheet = h("div", { class: "overlay sheet", role: "dialog", "aria-label": `Hand off ${t.key}` });
  const close = () => sheet.remove();
  const body = h("div", { class: "sheet-body" });

  function draw() {
    const members = state.snap.agents.filter((a) => a.group_name === f.group);
    const lead = members.find((a) => a.role === "orchestrator");
    const recipient = f.target || lead?.name || "";
    const codex = f.harness === "codex";
    if (codex && f.backend === "herdr") f.backend = "headless";
    const field = (label, el) => h("label", { class: "field" }, h("span", { class: "muted small" }, label), el);
    const modeBtn = (m, title, sub, disabled) => h("button", { class: `mode${f.mode === m ? " on" : ""}`, disabled,
      onclick: () => { f.mode = m; draw(); } }, h("strong", {}, title), h("span", { class: "muted small" }, sub));

    const opts = [];
    if (f.mode === "existing") {
      opts.push(field("Group", h("select", { onchange: (e) => { f.group = e.target.value; f.target = ""; draw(); } },
        groups.map((g) => h("option", { value: g, selected: g === f.group }, g)))));
      opts.push(field("Send to", h("select", { onchange: (e) => { f.target = e.target.value; draw(); } },
        lead ? h("option", { value: "", selected: !f.target }, `${lead.name} (lead)`) : h("option", { value: "", disabled: true, selected: !f.target }, "Choose an agent…"),
        members.filter((a) => a.role !== "orchestrator").map((a) => h("option", { value: a.name, selected: a.name === f.target }, a.name)))));
      if (recipient) {
        const r = members.find((a) => a.name === recipient);
        opts.push(h("p", { class: "muted small" }, `${recipient} gets the ticket${r?.state === "working" ? " as a message it reads when it finishes what it's doing" : " as its next prompt"}${r?.role === "orchestrator" ? ", and brings in the group's agents" : ""}.`));
      }
    } else {
      const name = h("input", { value: f.name, autocapitalize: "off", autocorrect: "off", spellcheck: "false",
        oninput: (e) => { f.name = e.target.value = typedName(e.target.value); start.disabled = !ready(); } });
      opts.push(field(f.mode === "group" ? "Group name" : "Agent name", name));
      opts.push(field("Working directory (repo)", h("input", { value: f.cwd, autocapitalize: "off", autocorrect: "off", spellcheck: "false", oninput: (e) => (f.cwd = e.target.value) })));
      const defaultModel = (codex ? d.codex_model : d.model) || (codex ? "Codex default" : "Claude Code default");
      opts.push(h("details", {}, h("summary", {}, `Run with ${codex ? "Codex" : "Claude Code"}, ${f.backend === "herdr" ? "in a herdr pane" : "headless"}${f.model ? `, on ${f.model}` : ""}`),
        field("Run with", h("select", { onchange: (e) => { f.harness = e.target.value; draw(); } },
          h("option", { value: "claude", selected: !codex }, "Claude Code"),
          h("option", { value: "codex", selected: codex, disabled: !pipe?.codex }, `Codex (experimental)${pipe?.codex ? "" : " · not found"}`))),
        field("Run in", h("select", { onchange: (e) => { f.backend = e.target.value; draw(); } },
          h("option", { value: "headless", selected: f.backend === "headless" }, "Headless (managed by drovr)"),
          h("option", { value: "herdr", selected: f.backend === "herdr", disabled: codex || !state.snap.herdr }, `herdr pane${codex ? " · Claude Code only" : state.snap.herdr ? "" : " · herdr not found"}`))),
        field(f.mode === "group" ? "Lead's model" : "Model", h("input", { value: f.model, placeholder: defaultModel, autocapitalize: "off", autocorrect: "off", spellcheck: "false", oninput: (e) => (f.model = e.target.value) }))));
    }
    const notes = h("textarea", { rows: 4, placeholder: "Constraints, approach, where to look, what not to touch…", oninput: (e) => (f.notes = e.target.value) });
    notes.value = f.notes;

    const ready = () => !busy && (f.mode === "existing" ? !!recipient : !!f.name);
    const start = h("button", { class: "primary", disabled: !ready(), onclick: async () => {
      busy = true;
      start.disabled = true;
      start.textContent = "Starting…";
      try {
        const r = await call("dispatch", { key: t.key, ...f, model: f.model.trim() });
        close();
        toast(f.mode === "existing" ? `${r.ticket} sent to ${recipient}` : `${r.ticket} handed off to ${r.agent}`);
        await refresh();
        go(agentHash(r.agent));
      } catch (e) {
        toast(String(e.message || e), true);
        busy = false;
        start.disabled = !ready();
        start.textContent = "Start";
      }
    } }, busy ? "Starting…" : "Start");

    fill(body,
      h("p", { class: "muted", style: "margin-top: 0" }, t.summary),
      h("div", { class: "modes" },
        modeBtn("solo", "Single agent", "One agent works the ticket and reports back."),
        modeBtn("group", "New group + lead", "A lead plans the work and spawns workers."),
        modeBtn("existing", "Existing group", groups.length ? "Its lead brings in the others." : "You have no groups yet.", !groups.length)),
      opts,
      field("Notes for the agent", notes),
      h("div", { class: "actions" }, start, h("button", { onclick: close }, "Cancel")));
  }

  draw();
  put(sheet,
    h("div", { class: "bar" }, h("button", { class: "ghost", onclick: close }, "‹ Back"), h("strong", { class: "grow ellipsis" }, `Hand off ${t.key}`)),
    body);
  document.body.append(sheet);
}

// -- start ---------------------------------------------------------------------

refresh().then(route);
