// The Dock badge and notifications for the Inbox and the PRs view.

import { getCurrentWindow } from "@tauri-apps/api/window";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Item, ReviewPr } from "./api";

/** Inbox items seen so far; null until the first snapshot, so what's
 *  already waiting when drovr starts doesn't notify. */
let known: Set<number> | null = null;
/** PRs (by key) seen as `new`, the same way. */
let knownPrs: Set<string> | null = null;
let inboxCount = 0;
let newPrCount = 0;
let badge = -1;
let allowed: boolean | null = null;

/** The Dock badge is the Inbox count plus the PRs still waiting on you. */
function setBadge() {
  const n = inboxCount + newPrCount;
  if (n === badge) return;
  badge = n;
  getCurrentWindow().setBadgeCount(n || undefined).catch(() => {});
}

async function send(title: string, body: string) {
  try {
    allowed ??= (await isPermissionGranted()) || (await requestPermission()) === "granted";
    if (!allowed) return;
    sendNotification({ title, body });
  } catch {}
}

/** Keep the Dock badge up to date, and notify about new items while
 *  you're in another app. */
export async function inboxChanged(items: Item[], notify: boolean) {
  inboxCount = items.length;
  setBadge();
  const fresh = known ? items.filter((i) => !known!.has(i.id)) : [];
  known = new Set(items.map((i) => i.id));
  if (!fresh.length || !notify || document.hasFocus()) return;
  await send(
    fresh.length === 1 ? `${fresh[0].agent} needs you` : `${fresh.length} new items in the Inbox`,
    fresh.length === 1 ? fresh[0].title : fresh.map((i) => `${i.agent}: ${i.title}`).join("\n"),
  );
}

const prName = (p: ReviewPr) => `${p.owner}/${p.repo}#${p.number}`;
const prLine = (p: ReviewPr) => (p.title ? `${prName(p)}: ${p.title}` : prName(p));

/** Notify about PRs that newly wait on you, and count them in the Dock badge. */
export async function prsChanged(rows: ReviewPr[], notify: boolean) {
  const waiting = rows.filter((r) => r.status === "new");
  newPrCount = waiting.length;
  setBadge();
  const fresh = knownPrs ? waiting.filter((r) => !knownPrs!.has(r.key)) : [];
  // Every open PR, so one whose dispatch failed and went back to New
  // doesn't notify again.
  knownPrs = new Set(rows.map((r) => r.key));
  if (!fresh.length || !notify || document.hasFocus()) return;
  await send(
    fresh.length === 1 ? "PR to review" : `${fresh.length} PRs to review`,
    fresh.length === 1 ? prLine(fresh[0]) : fresh.map(prLine).join("\n"),
  );
}
