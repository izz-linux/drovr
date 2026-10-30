// The Dock badge and notifications for the Inbox.

import { getCurrentWindow } from "@tauri-apps/api/window";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Item } from "./api";

/** Inbox items seen so far; null until the first snapshot, so what's
 *  already waiting when drovr starts doesn't notify. */
let known: Set<number> | null = null;
let badge = -1;
let allowed: boolean | null = null;

/** Keep the Dock badge at the Inbox count, and notify about new items while
 *  you're in another app. */
export async function inboxChanged(items: Item[], notify: boolean) {
  if (items.length !== badge) {
    badge = items.length;
    getCurrentWindow().setBadgeCount(badge || undefined).catch(() => {});
  }
  const fresh = known ? items.filter((i) => !known!.has(i.id)) : [];
  known = new Set(items.map((i) => i.id));
  if (!fresh.length || !notify || document.hasFocus()) return;
  try {
    allowed ??= (await isPermissionGranted()) || (await requestPermission()) === "granted";
    if (!allowed) return;
    sendNotification(
      fresh.length === 1
        ? { title: `${fresh[0].agent} needs you`, body: fresh[0].title }
        : { title: `${fresh.length} new items in the Inbox`, body: fresh.map((i) => `${i.agent}: ${i.title}`).join("\n") },
    );
  } catch {}
}
