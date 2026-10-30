// Terminals under the agent output. A group shares one, which opens in your
// home folder like a new iTerm2 window; a solo agent's opens in the repo it
// worked in most recently. The xterm.js instances live here, outside any
// component, so switching agents or hiding the panel keeps each scrollback
// and the shells keep running.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { listen } from "@tauri-apps/api/event";
import { api, type Agent } from "./api";

type Entry = {
  term: Terminal; fit: FitAddon; el: HTMLDivElement; running: boolean; ready: Promise<void>;
  /** When the shell last printed something; 0 until it has since starting. */
  lastOutput: number;
};

const terms = new Map<string, Entry>();
/** Commands typed in from the Inbox, waiting for you to press Enter. */
const waiting = new Map<string, (mark: number) => void>();
let settings: Awaited<ReturnType<typeof api.termSettings>> | null = null;
let listening = false;

/** Which terminal an agent uses: its group's, or its own. */
export function termKey(a: Agent): string {
  return a.group_name ? `group:${a.group_name}` : `agent:${a.name}`;
}

export function termLabel(a: Agent): string {
  return a.group_name ? `${a.group_name} terminal` : `${a.name} terminal`;
}

function css(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function palette() {
  return {
    background: css("--bg"),
    foreground: css("--text"),
    cursor: css("--accent"),
    selectionBackground: css("--accent") + "55",
  };
}

function listenOnce() {
  if (listening) return;
  listening = true;
  listen<{ key: string; data: string }>("term-data", (e) => {
    const t = terms.get(e.payload.key);
    if (!t) return;
    t.lastOutput = Date.now();
    t.term.write(e.payload.data);
  });
  listen<string>("term-exit", (e) => {
    const t = terms.get(e.payload);
    if (!t) return;
    t.running = false;
    t.term.write("\r\n\x1b[2m[shell exited; press Enter to start a new one]\x1b[0m\r\n");
  });
  // Follow theme changes.
  new MutationObserver(() => terms.forEach((t) => (t.term.options.theme = palette()))).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
}

async function start(key: string, agent: Agent, t: Entry) {
  const cwd = agent.group_name ? "" : await api.termWorkspace(agent.name).catch(() => agent.cwd);
  t.fit.fit();
  t.lastOutput = 0;
  await api.termOpen(key, cwd, t.term.cols, t.term.rows);
  t.running = true;
}

/** Shells whose line editor turns on bracketed paste once it's ready for a
 *  command. bash does too from 5.1, but macOS's /bin/bash is 3.2. */
const PASTE_READY = ["zsh", "fish"];

/** Wait until the shell is ready for a command, so text typed in isn't lost
 *  to (or read by) its startup files: zsh and fish turn on bracketed paste
 *  when their prompt is up; for other shells, or a bash that turns it on
 *  too, it's printed something and then gone quiet. False if it isn't ready
 *  within `timeout` ms. */
async function shellReady(t: Entry, timeout: number): Promise<boolean> {
  const strict = PASTE_READY.includes(settings?.shell ?? "");
  const until = Date.now() + timeout;
  while (Date.now() < until) {
    if (!t.running) return false;
    if (t.term.modes.bracketedPasteMode) return true;
    if (!strict && t.lastOutput && Date.now() - t.lastOutput > 1500) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return false;
}

/** The terminal for this agent, started if it isn't running. */
export async function terminalFor(agent: Agent): Promise<Entry> {
  listenOnce();
  settings ??= await api.termSettings().catch(() => ({ font_family: "", font_size: 13, option_as_meta: false, copy_on_select: true, shell: "" }));
  const key = termKey(agent);
  let t = terms.get(key);
  if (!t) {
    const term = new Terminal({
      fontFamily: settings.font_family ? `"${settings.font_family}", ${css("--mono")}` : css("--mono"),
      fontSize: settings.font_size || 13,
      macOptionIsMeta: settings.option_as_meta,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
      theme: palette(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon((_e, url) => api.openUrl(url)));
    const el = document.createElement("div");
    el.className = "term-host";
    term.onData((data) => {
      const entry = terms.get(key)!;
      const onEnter = data.includes("\r") ? waiting.get(key) : undefined;
      if (entry.running && onEnter) {
        // Output starts here, so the pasted command isn't captured twice.
        waiting.delete(key);
        api.termMark(key).then((mark) => { onEnter(mark); return api.termWrite(key, data); }).catch(() => {});
      } else if (entry.running) api.termWrite(key, data).catch(() => {});
      else if (data === "\r") entry.ready = start(key, agent, entry);
    });
    term.onResize(({ cols, rows }) => api.termResize(key, cols, rows).catch(() => {}));
    // Copy on select (terminal.copy_on_select), when the mouse lets go: a
    // drag, a double-click on a word or a triple-click on a line.
    el.addEventListener("mouseup", () => {
      if (!settings?.copy_on_select || !term.hasSelection()) return;
      const text = term.getSelection();
      if (text) navigator.clipboard.writeText(text).catch(() => {});
    });
    // Let ⌘ shortcuts (⌘1…⌘8, ⌘F, ⌘C/⌘V through the Edit menu) reach the app.
    term.attachCustomKeyEventHandler((e) => !e.metaKey);
    t = { term, fit, el, running: false, ready: Promise.resolve(), lastOutput: 0 };
    terms.set(key, t);
    t.ready = new Promise<void>((done) => {
      // Open once the element is in the page, so it can measure itself.
      requestAnimationFrame(() => {
        term.open(el);
        start(key, agent, t!).finally(done);
      });
    });
  }
  return t;
}

/** Type a command into an agent's terminal without running it, once its
 *  shell is ready (`onWait` is called if that takes a moment). `ran` gets
 *  where its output starts once you press Enter, so Mark as run can send that
 *  output to the agent. */
export async function typeInto(
  agent: Agent,
  text: string,
  ran: (m: { key: string; mark: number }) => void,
  onWait: () => void = () => {},
) {
  const t = await terminalFor(agent);
  await t.ready;
  const key = termKey(agent);
  // A shell that exited would drop the text: start a new one first.
  if (!t.running) {
    t.ready = start(key, agent, t);
    await t.ready;
  }
  if (!(await shellReady(t, 300))) {
    onWait();
    if (!(await shellReady(t, 20000))) {
      throw new Error(
        "Your shell hasn't shown its prompt after 20 seconds, so the command wasn't typed in. Something in its startup files may be waiting; check the terminal, then use Run in terminal again.",
      );
    }
  }
  waiting.set(key, (mark) => ran({ key, mark }));
  t.term.paste(text);
  t.term.focus();
}

/** Force-stop what's running in the agent's terminal, for a job that
 *  ignores ^C. The shell and its scrollback stay. */
export async function stopJob(agent: Agent): Promise<boolean> {
  const key = termKey(agent);
  if (!terms.get(key)?.running) return false;
  return api.termStopJob(key);
}

/** End the agent's shell and everything running in it, and start a new
 *  one. The scrollback stays above a marker. */
export async function restartShell(agent: Agent) {
  const key = termKey(agent);
  const t = terms.get(key);
  if (!t) return;
  waiting.delete(key);
  await api.termClose(key);
  t.running = false;
  t.term.write("\r\n\x1b[2m[shell restarted]\x1b[0m\r\n");
  t.ready = start(key, agent, t);
  await t.ready;
  t.term.focus();
}

/** Put a terminal back on the page (switching agents, showing the panel
 *  again). The browser resets the scroll position of an element taken off
 *  the page, but xterm.js keeps its own copy and only re-syncs when that
 *  changes, so without this the next scroll jumps to the top and the end
 *  of the output is out of reach until new output arrives. */
export function attach(t: Entry, box: HTMLElement) {
  const b = t.term.buffer.active;
  const atBottom = b.viewportY >= b.baseY;
  const line = b.viewportY;
  box.replaceChildren(t.el);
  requestAnimationFrame(() => {
    fitAll();
    const viewport = t.el.querySelector<HTMLElement>(".xterm-viewport");
    if (!viewport) return;
    const rows = t.term.buffer.active.length;
    // Where xterm thinks it is, in the viewport's own pixels.
    viewport.scrollTop = atBottom ? viewport.scrollHeight : (line * viewport.scrollHeight) / Math.max(rows, 1);
  });
}

export function fitAll() {
  terms.forEach((t) => {
    if (t.el.isConnected) t.fit.fit();
  });
}

/** Single-quote for the shell. */
export function shq(s: string): string {
  return /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}
