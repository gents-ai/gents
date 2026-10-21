// Chat sends whose delivery outcome is unknown. The owner may already hold the
// signed request, so the same message must reconcile that exact request rather
// than be sent again under a new identity. Entries persist across reloads so a
// restart cannot silently make a fresh resend look safe; only identifiers and a
// content fingerprint are stored, never the signed request.

const KEY = "gents.chat.unresolvedSubmissions.v1";

export type UnresolvedChatSubmission = {
  nodeDid: string;
  requestId: string;
  sessionId: string;
  /** The session selected when the send started (null for a new chat). */
  selectedSessionId: string | null;
  contentKey: string;
};

export function contentKey(content: string): string {
  let hash = 0x811c9dc5;
  for (const char of content.trim()) {
    hash ^= char.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

function isEntry(value: unknown): value is UnresolvedChatSubmission {
  if (!value || typeof value !== "object") return false;
  const entry = value as Record<string, unknown>;
  return (
    typeof entry.nodeDid === "string" &&
    typeof entry.requestId === "string" &&
    typeof entry.sessionId === "string" &&
    (typeof entry.selectedSessionId === "string" || entry.selectedSessionId === null) &&
    typeof entry.contentKey === "string"
  );
}

export function loadUnresolvedSubmissions(): UnresolvedChatSubmission[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter(isEntry) : [];
  } catch {
    return [];
  }
}

function store(entries: UnresolvedChatSubmission[]) {
  try {
    if (entries.length === 0) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, JSON.stringify(entries));
  } catch {
    // Native storage retains the signed request; this browser-side record
    // identifies which request the composer should reconcile after reload.
  }
}

export function rememberUnresolvedSubmission(entry: UnresolvedChatSubmission) {
  store([
    ...loadUnresolvedSubmissions().filter(
      (existing) => existing.requestId !== entry.requestId,
    ),
    entry,
  ]);
}

export function forgetUnresolvedSubmission(requestId: string) {
  store(loadUnresolvedSubmissions().filter((entry) => entry.requestId !== requestId));
}

/** The unresolved send this message would duplicate, if any. */
export function unresolvedSubmissionFor(
  nodeDid: string,
  selectedSessionId: string | null,
  content: string,
): UnresolvedChatSubmission | undefined {
  const key = contentKey(content);
  return loadUnresolvedSubmissions().find(
    (entry) =>
      entry.nodeDid === nodeDid &&
      entry.contentKey === key &&
      (entry.sessionId === selectedSessionId ||
        entry.selectedSessionId === selectedSessionId),
  );
}
