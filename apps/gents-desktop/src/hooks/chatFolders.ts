import { isWorkingNode } from "../ui/lib/nodes";
import type { ChatStore } from "./chatStore";
import { listedSession, nodeOf, type FleetState } from "./fleetStore";

const KEY = "gents.chatFolders";
const NEW_CHAT = "";
const NONE = "";

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
  if (folder) return { ...rest, [sessionId ?? NEW_CHAT]: folder };
  /* a session keeps "no folder" as a choice, so the folder it last ran in
     doesn't come back in its place */
  return sessionId ? { ...rest, [sessionId]: NONE } : rest;
}

export function adoptNewChatFolder(
  folders: ChatFolders,
  sessionId: string,
): ChatFolders {
  const pick = folders[NEW_CHAT];
  if (!pick) return folders;
  return withFolder(withFolder(folders, null, null), sessionId, pick);
}

export function recentFolders(folders: ChatFolders, limit = 5): string[] {
  return [...new Set(Object.values(folders).filter(Boolean).reverse())].slice(0, limit);
}

export function folderOf(
  folders: ChatFolders,
  sessionId: string | null,
): string | null {
  return folders[sessionId ?? NEW_CHAT] || null;
}

/** The folder a session's next message runs in: the one picked on this
    computer, or else, for a session on this computer's node, the one its
    latest request ran in. A folder another computer's node recorded is never
    used here, since a message may name only a folder on this computer. */
export function sessionFolder(
  folders: ChatFolders,
  fleet: FleetState,
  homeDid: string | null | undefined,
  nodeDid: string | null | undefined,
  sessionId: string | null,
): string | null {
  const picked = folders[sessionId ?? NEW_CHAT];
  if (picked !== undefined || !sessionId) return picked || null;
  const node = nodeDid ? nodeOf(fleet, nodeDid) : null;
  if (!node || !isWorkingNode(node, homeDid)) return null;
  return listedSession(fleet, nodeDid, sessionId)?.cwd ?? null;
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
