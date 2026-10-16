/* Per-viewer localStorage convenience; when absent the agent's own tool root
   applies. A new chat holds its pick under the empty key until the first send
   gives the session an id. */
import type { ChatStore } from "./chatStore";

const KEY = "gents.chatFolders";
const NEW_CHAT = "";

export type ChatFolders = Record<string, string>;

export function loadChatFolders(): ChatFolders {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, string] => typeof entry[1] === "string",
      ),
    );
  } catch {
    return {};
  }
}

function saveChatFolders(folders: ChatFolders) {
  try {
    localStorage.setItem(KEY, JSON.stringify(folders));
  } catch {}
}

export function withFolder(
  folders: ChatFolders,
  sessionId: string | null,
  folder: string | null,
): ChatFolders {
  const { [sessionId ?? NEW_CHAT]: _dropped, ...rest } = folders;
  return folder ? { ...rest, [sessionId ?? NEW_CHAT]: folder } : rest;
}

export function adoptNewChatFolder(
  folders: ChatFolders,
  sessionId: string,
): ChatFolders {
  const pick = folders[NEW_CHAT];
  if (!pick) return folders;
  return withFolder(withFolder(folders, null, null), sessionId, pick);
}

/* Picks are re-inserted on every change, so the map's order is the order
   they were last made in. */
/** Distinct folders, the most recently picked first. */
export function recentFolders(folders: ChatFolders, limit = 5): string[] {
  return [...new Set(Object.values(folders).reverse())].slice(0, limit);
}

/** The folder chosen for a session, or for the new chat under null. */
export function folderOf(
  folders: ChatFolders,
  sessionId: string | null,
): string | null {
  return folders[sessionId ?? NEW_CHAT] ?? null;
}

export function writeChatFolders(
  chat: ChatStore,
  next: (current: ChatFolders) => ChatFolders,
) {
  const current = chat.getState().folders;
  const updated = next(current);
  if (updated === current) return;
  saveChatFolders(updated);
  chat.setState({ folders: updated });
}
