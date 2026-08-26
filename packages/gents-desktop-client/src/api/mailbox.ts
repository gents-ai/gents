import { getDesktopApiAdapter } from "./adapter.js";
import type { DesktopApiAdapter } from "./types.js";

export function listMailbox(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).listMailbox();
}

export function startMailboxRequest(itemId: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).startMailboxRequest(itemId);
}

export function dismissMailboxItem(itemId: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).dismissMailboxItem(itemId);
}
