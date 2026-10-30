import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

export type Agent = {
  name: string; group_name: string; role: string; backend: string; harness: string; run_state: string; state: string;
  status: string; cwd: string; session_id: string; pane: string; ticket: string; task: string;
  runs: number; cost_usd: number; pending: number; created: number; updated: number;
  /** Its stage in the ticket workflow (triage, build, …); empty outside it. */
  stage: string;
  /** The model it runs on; empty for the configured default. */
  model: string;
  /** Your messages it hasn't read yet. */
  queued: Message[];
};
export type Group = {
  name: string; goal: string; ticket: string; cwd: string; plans_dir: string; created: number;
  /** Created with the ticket workflow on: its lead runs the ticket in stages. */
  workflow: boolean;
};
export type NewGroup = { name: string; goal: string; ticket: string; cwd: string; backend: string; harness: string; plans_dir: string; lead: boolean };
export type Defaults = {
  cwd: string; backend: string; harness: string; model: string; permission_mode: string; worktrees: boolean; plans_dir: string; your_name: string;
  notify: boolean; cost_warning_usd: number; copy_on_select: boolean;
  /** workflow.enabled: the ticket record and timeline are on. */
  workflow: boolean;
  /** jira.ticket_skill: how agents write a ticket for their own task. */
  ticket_skill: string;
};
export type PlansFolder = { name: string; prompts: string[] };
export type Item = {
  id: number; ts: number; agent: string; kind: string; title: string; body: string;
  options: string[]; payload: any; status: string; answer: string; delivered: boolean;
};
export type Snapshot = { agents: Agent[]; groups: Group[]; inbox: Item[]; recent: Item[]; unseen_reports: number; herdr: boolean; codex: boolean };
export type ReportFile = { path: string; folder: string; title: string; modified: number; agent: string; summary: string; seen: boolean };
export type Line = { kind: string; text: string };
export type Message = { id: number; ts: number; sender: string; recipient: string; group_name: string; body: string };
/** A ticket with a record (`ledger.jsonl`) in the plans folder. */
export type TicketRecord = { ticket: string; folder: string; path: string; events: number; updated: number };
/** One event in a ticket's record: `ts`, `at`, `ticket`, `kind`, `agent`, … */
export type RecordEvent = { ts: number; at: string; ticket: string; kind: string; agent?: string; group?: string; role?: string; [k: string]: any };
/** A ticket in your sprint, as the watcher last saw it. */
export type SprintTicket = {
  key: string; summary: string; status: string; category: string; assignee: string; priority: string; issue_type: string;
  url: string; updated: string; comments: number; last_comment: string;
};
/** A pull request whose title names one of the sprint's tickets. */
export type WatchedPr = {
  url: string; repo: string; number: number; title: string; tickets: string[]; state: string; draft: boolean;
  ci: string; failing: string[]; failing_urls: string[]; review: string; mergeable: string; head: string; updated: string;
};
export type WatchState = {
  jira: { at: number; error: string }; github: { at: number; error: string };
  tickets: Record<string, SprintTicket>; prs: Record<string, WatchedPr>;
};
/** One thing a retro proposes, with its evidence and what you decided. */
export type Proposal = {
  id: string; kind: string; title: string; why: string; evidence: string[]; change: any;
  status: "pending" | "approved" | "dismissed"; result: string;
};
export type Retro = { dir: string; tickets: string[]; started: number; agent: string; note: string; proposals: Proposal[] };
export type Note = { key: string; value: string; author: string; ts: number };
export type Ticket = {
  key: string; summary: string; status: string; assignee: string; priority: string; issue_type: string;
  url: string; description: string; comments: string[];
};
export type Page = { id: string; title: string; space: string; url: string; excerpt: string; body: string };
export type SlackMessage = { channel: string; user: string; ts: string; text: string; permalink: string };
export type Change = { path: string; status: string; uncommitted: boolean; is_html: boolean };
export type Repo = {
  root: string; label: string; summary: string; base: string; changes: Change[]; html: string[];
  /** In a repo every agent's work shares (the plans folder's): the parts this agent worked in. Empty: all of it. */
  scope: string[];
};
export type NewAgent = {
  name: string; group?: string; role?: string; backend?: string; harness?: string; cwd?: string; ticket?: string; task: string;
  /** Start from this existing Claude Code session. */
  resume_session?: string;
  /** Continue in a copy of that session, leaving the original untouched. */
  fork?: boolean;
  /** With no ticket: the agent writes and creates one first (jira.ticket_skill), then takes it. */
  create_ticket?: boolean;
  /** What you want in that ticket. */
  ticket_notes?: string;
};
export type ClaudeSession = { id: string; cwd: string; first_prompt: string; modified: number };

export const api = {
  snapshot: () => invoke<Snapshot>("snapshot"),
  output: (name: string, lines = 600) => invoke<Line[]>("agent_output", { name, lines }),
  newAgent: (spec: NewAgent) => invoke<Agent>("new_agent", { spec }),
  createGroup: (spec: NewGroup) => invoke<Agent | null>("create_group", { spec }),
  addLead: (group: string, goal: string, backend: string, harness = "") => invoke<Agent>("add_lead", { group, goal, backend, harness }),
  defaults: () => invoke<Defaults>("defaults"),
  claudeSessions: () => invoke<ClaudeSession[]>("claude_sessions"),
  findSession: (id: string) => invoke<ClaudeSession | null>("find_session", { id }),
  plansFolders: () => invoke<PlansFolder[]>("plans_folders"),
  readPrompt: (folder: string, file: string) => invoke<string>("read_prompt", { folder, file }),
  prompt: (name: string, text: string) => invoke<string>("prompt_agent", { name, text }),
  /** Stop the agent's run if it has one and deliver queued message `id` now. */
  sendNow: (name: string, id: number) => invoke<string>("send_now", { name, id }),
  /** Take a queued message back before the agent reads it; false if it already has. */
  unqueue: (name: string, id: number) => invoke<boolean>("unqueue", { name, id }),
  stop: (name: string) => invoke<void>("stop_agent", { name }),
  stopGroup: (group: string) => invoke<void>("stop_group", { group }),
  resume: (name: string, text = "") => invoke<string>("resume_agent", { name, text }),
  /** Returns a note about anything left behind (a worktree with uncommitted changes). */
  remove: (name: string) => invoke<string>("remove_agent", { name }),
  removeGroup: (group: string) => invoke<string>("remove_group", { group }),
  attachCommand: (name: string) => invoke<string>("attach_command", { name }),
  openInHerdr: (name: string) => invoke<string>("open_in_herdr", { name }),
  answer: (id: number, answer: string) => invoke<void>("inbox_answer", { id, answer }),
  reject: (id: number, reason: string) => invoke<void>("inbox_reject", { id, reason }),
  termWorkspace: (name: string) => invoke<string>("term_workspace", { name }),
  termSettings: () => invoke<{ font_family: string; font_size: number; option_as_meta: boolean; copy_on_select: boolean; shell: string }>("term_settings"),
  termOpen: (key: string, cwd: string, cols: number, rows: number) => invoke<boolean>("term_open", { key, cwd, cols, rows }),
  termWrite: (key: string, data: string) => invoke<void>("term_write", { key, data }),
  termResize: (key: string, cols: number, rows: number) => invoke<void>("term_resize", { key, cols, rows }),
  termMark: (key: string) => invoke<number>("term_mark", { key }),
  termSince: (key: string, mark: number, max: number) => invoke<string>("term_since", { key, mark, max }),
  /** Force-stop the terminal's foreground job; false if nothing was running. */
  termStopJob: (key: string) => invoke<boolean>("term_stop_job", { key }),
  termClose: (key: string) => invoke<void>("term_close", { key }),
  markRan: (id: number, note: string) => invoke<void>("inbox_mark_ran", { id, note }),
  dismiss: (id: number) => invoke<void>("inbox_dismiss", { id }),
  runCommand: (id: number, target: string, command: string) => invoke<string>("run_command", { id, target, command }),
  bus: (group: string) => invoke<{ messages: Message[]; notes: Note[] }>("bus", { group }),
  send: (to: string, group: string, body: string) => invoke<void>("send_message", { to, group, body }),
  jiraSearch: (query: string) => invoke<Ticket[]>("jira_search", { query }),
  jiraGet: (key: string) => invoke<Ticket>("jira_get", { key }),
  dispatch: (ticket: Ticket, mode: string, name: string, cwd: string, backend: string, harness: string, notes: string, group = "", target = "") =>
    invoke<Agent>("dispatch_ticket", { ticket, mode, name, cwd, backend, harness, notes, group, target }),
  confluenceSearch: (query: string) => invoke<Page[]>("confluence_search", { query }),
  confluenceGet: (id: string) => invoke<Page>("confluence_get", { id }),
  cancelLookups: () => invoke<number>("cancel_lookups"),
  slackSearch: (query: string) => invoke<SlackMessage[]>("slack_search", { query }),
  ask: (question: string) => invoke<string>("ask", { question }),
  review: (name: string) => invoke<{ cwd: string; repos: Repo[] }>("review", { name }),
  diff: (root: string, path: string, scope: string[] = []) => invoke<string>("review_diff", { root, path, scope }),
  configInfo: () => invoke<{ path: string; home: string; text: string; effective: string }>("config_info"),
  saveConfig: (text: string) => invoke<void>("save_config", { text }),
  reports: () => invoke<{ dir: string; files: ReportFile[] }>("reports"),
  reportSeen: (path: string) => invoke<void>("report_seen", { path }),
  reportsAllSeen: () => invoke<void>("reports_all_seen"),
  openUrl: (url: string) => openUrl(url),
  /** A folder (in Finder) or an HTML page (in your browser), from the plans
   *  folder or a reviewed repo; drovr checks it won't run anything. */
  openPath: (path: string) => invoke<void>("open_path", { path }),
  records: () => invoke<TicketRecord[]>("records"),
  timeline: (ticket: string) => invoke<RecordEvent[]>("timeline", { ticket }),
  watchState: () => invoke<WatchState>("watch_state"),
  /** Read Jira and GitHub now (can take a while). */
  watchNow: () => invoke<WatchState>("watch_now"),
  startRetro: (tickets: string[], note = "") => invoke<Agent>("start_retro", { tickets, note }),
  retros: () => invoke<Retro[]>("retros"),
  /** Approve (and apply) or dismiss a proposal; returns what approving did. */
  decideProposal: (dir: string, id: string, approve: boolean) => invoke<string>("decide_proposal", { dir, id, approve }),
  /** A preview URL for a local file. `convertFileSrc` encodes the slashes
   *  too, so a page's relative links (`style.css`, `img/chart.png`) resolved
   *  to the protocol's root; only the leading one is encoded here, which the
   *  protocol still reads as an absolute path. */
  fileSrc: (path: string) => convertFileSrc("") + path.split("/").map(encodeURIComponent).join("/").replace(/^\//, "%2F"),
};

/** Ticket keys in `text`, uppercased and in order (the same rule drovr's record uses). */
export function ticketsIn(text: string): string[] {
  const keys = (text.match(/(?<![A-Za-z0-9-])[A-Za-z][A-Za-z0-9]+-\d+(?![A-Za-z0-9-])/g) ?? []).map((k) => k.toUpperCase());
  return [...new Set(keys)];
}

export function ago(ts: number): string {
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 60) return `${Math.floor(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}
