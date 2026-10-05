/* The folder the user works in, per chat. A new chat holds its pick under
   the empty key until the first send gives the session an id; every later
   message of that session carries the same folder. Kept in the webview's
   storage, which is a per-viewer convenience: without it the chat simply
   has no folder and the agent's own tool root applies. */
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
  } catch {
    /* storage unavailable: the pick lasts for this run only */
  }
}

/* the folder after `folder` is chosen (or cleared with null) for `sessionId` */
export function withFolder(
  folders: ChatFolders,
  sessionId: string | null,
  folder: string | null,
): ChatFolders {
  const { [sessionId ?? NEW_CHAT]: _dropped, ...rest } = folders;
  return folder ? { ...rest, [sessionId ?? NEW_CHAT]: folder } : rest;
}

/* the new chat's pick, handed to the session the first send created */
export function adoptNewChatFolder(
  folders: ChatFolders,
  sessionId: string,
): ChatFolders {
  const pick = folders[NEW_CHAT];
  if (!pick) return folders;
  return withFolder(withFolder(folders, null, null), sessionId, pick);
}

/** The folder chosen for a session, or for the new chat under null. */
export function folderOf(
  folders: ChatFolders,
  sessionId: string | null,
): string | null {
  return folders[sessionId ?? NEW_CHAT] ?? null;
}

/** Commits the folders and keeps them for the next run. */
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
