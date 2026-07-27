/** Minimal view shapes used by chat projections (host may pass fuller objects). */
export type ConversationSummary = {
  sessionId: string;
  title?: string | null;
  previewText?: string | null;
  status?: string | null;
  behaviorId?: string | null;
  latestRequestId?: string | null;
  turnState?: string | null;
  messageCount?: number;
  toolCallCount?: number;
  [key: string]: unknown;
};

/** Session snapshot is intentionally open — projection only reads a few keys. */
export type DesktopSessionSnapshot = {
  sessionId?: string | null;
  requestId?: string | null;
  turnState?: string | null;
  latestRequestId?: string | null;
  pendingTurn?: { requestId?: string | null; [key: string]: unknown } | null;
  [key: string]: unknown;
};
