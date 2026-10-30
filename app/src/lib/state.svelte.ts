import { api, type Defaults, type Page, type SlackMessage, type Snapshot, type Ticket } from "./api";
import { inboxChanged } from "./notify";

/** The Jira view's search and open ticket, kept while you visit other views. */
export const jira = $state({
  query: "",
  results: [] as Ticket[],
  ticket: null as Ticket | null,
  loading: "",
  error: "",
});

/** The Slack · Confluence view's search, kept (and still running) while you
 *  visit other views. */
export const search = $state({
  source: "slack" as "slack" | "confluence" | "ask",
  query: "",
  loading: false,
  error: "",
  slack: [] as SlackMessage[],
  pages: [] as Page[],
  page: null as Page | null,
  answer: "",
  /** The agent results go to; when set, a finished search is sent to it. */
  target: "",
});

export type View = "agents" | "inbox" | "reports" | "jira" | "search" | "bus" | "review" | "settings" | "pipeline";

export const app = $state({
  view: "agents" as View,
  snap: { agents: [], groups: [], inbox: [], recent: [], unseen_reports: 0, herdr: false, codex: false } as Snapshot,
  selected: "" as string,
  error: "" as string,
  toast: "" as string,
  /** A ticket waiting to be dispatched from the dispatch dialog. */
  dispatching: null as Ticket | null,
  newAgentOpen: false,
  newGroupOpen: false,
  /** What agents call you (defaults.your_name, else "the human"). */
  you: "the human",
  /** Bumped by Edit → Find… (⌘F); the current view opens its search. */
  find: 0,
  /** The terminal panel under an agent's output (View → Toggle Terminal, ⌃`). */
  termOpen: false,
  /** A command from the Inbox waiting to be typed into the agent's terminal. */
  termRun: null as { agent: string; itemId: number; text: string } | null,
  /** Where each Inbox command's output starts in its terminal, for Mark as run. */
  termMarks: {} as Record<number, { key: string; mark: number }>,
  /** Group preselected when the New agent dialog opens. */
  newAgentGroup: "",
  /** The keyboard shortcut list (View → Keyboard Shortcuts, ⌘/). */
  shortcutsOpen: false,
  /** The ticket whose record (timeline) is open; empty when closed. */
  timeline: "",
  /** Config the window uses (cost warning, notifications), loaded at start. */
  defaults: null as Defaults | null,
});

export async function refresh() {
  try {
    app.snap = await api.snapshot();
    app.error = "";
    inboxChanged(app.snap.inbox, app.defaults?.notify ?? true);
  } catch (e) {
    app.error = String(e);
  }
}

export function toast(msg: string) {
  app.toast = msg;
  setTimeout(() => { if (app.toast === msg) app.toast = ""; }, 3500);
}

/** Run an action, surface its error as a toast, refresh state afterwards. */
export async function act<T>(fn: () => Promise<T>, ok?: string): Promise<T | undefined> {
  try {
    const r = await fn();
    if (ok) toast(ok);
    await refresh();
    return r;
  } catch (e) {
    toast(String(e));
  }
}

export function select(name: string, view: View = "agents") {
  app.selected = name;
  app.view = view;
}

/** Run the Slack · Confluence view's search. It finishes even if you leave
 *  the view, and goes to the agent picked under "Send results to" (picked
 *  when it finishes, so you can choose one while it runs). */
export async function runSearch() {
  const q = search.query.trim();
  const source = search.source;
  if (!q || search.loading) return;
  search.loading = true;
  search.error = "";
  let text = "";
  try {
    if (source === "slack") {
      search.slack = await api.slackSearch(q);
      text = search.slack.length
        ? `Slack search for "${q}" (${search.slack.length} results):\n\n` +
          search.slack.map((m) => `- #${m.channel} (${m.user}): ${m.text.replace(/\s+/g, " ").slice(0, 400)}${m.permalink ? `\n  ${m.permalink}` : ""}`).join("\n")
        : "";
    } else if (source === "confluence") {
      search.pages = await api.confluenceSearch(q);
      search.page = null;
      text = search.pages.length
        ? `Confluence search for "${q}" (${search.pages.length} pages):\n\n` +
          search.pages.map((p) => `- ${p.title} (${p.space})${p.url ? ` ${p.url}` : ""}${p.excerpt ? `\n  ${p.excerpt.replace(/\s+/g, " ").slice(0, 300)}` : ""}`).join("\n")
        : "";
    } else {
      search.answer = await api.ask(q);
      text = search.answer ? `Answer to "${q}":\n\n${search.answer}` : "";
    }
  } catch (e) {
    search.error = String(e);
  }
  search.loading = false;
  const to = search.target;
  if (to && text) await act(() => api.send(to, "", text), `${source === "ask" ? "Answer" : "Results"} sent to ${to}`);
  else if (app.view !== "search" && !search.error) toast(`${source === "ask" ? "Answer" : "Search results"} ready in Slack · Confluence`);
}
