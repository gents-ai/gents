import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  SessionRowsView,
  SessionRowsWindow,
} from "@source-inc/gents-desktop-client";
import {
  BridgeInvokeError,
  applySessionLiveDelta,
  sessionLiveDeltaRequest,
} from "@source-inc/gents-desktop-client";
import { acceptsAsyncResult } from "./observationOrdering";
import { createContextDetailsCache } from "./sessionContextDetails";
import { timingConfig } from "./timing";
import { mergeSessionTipSnapshot } from "@source-inc/gents-desktop-client";
import type { SessionLoadState } from "../lib/loadingStatus";
import type { SelectionStore } from "./selectionStore";
import {
  IDLE_LOAD,
  readSession,
  writeSession,
  writeSessionLoad,
  type SessionStore,
} from "./sessionStore";

export const SESSION_TIMELINE_PAGE_SIZE = 40;
/* the bridge's largest rows page */
const TRANSCRIPT_READ_SIZE = 80;
/** how long a session's reads must find nothing new before they slow down */
const QUIET_AFTER_MS = 60_000;
/** the wait between reads of a session that has gone quiet */
const QUIET_READ_GAP_MS = 8_000;

/* a session's content apart from the store-wide revision, which moves when
   any document in the store changes */
const sessionSignature = (snapshot: DesktopSessionSnapshot) =>
  JSON.stringify(snapshot, (key, value) =>
    key === "projectionRevision" ? undefined : value,
  );

type SessionReadParams = {
  api: DesktopApiAdapter;
  store: SelectionStore;
  sessionStore: SessionStore;
  trackedRequestId: () => string | null;
  setError: (error: string | null) => void;
};

export function createSessionReads({
  api,
  store,
  sessionStore,
  trackedRequestId,
  setError,
}: SessionReadParams) {
  const loadSessionContextDetails = createContextDetailsCache(
    (request) => api.fetchSessionContextDetails?.(request) ?? Promise.resolve(null),
  );
  let refreshSeq = 0;
  let liveSeq = 0;
  let runEpoch = 0;
  let readAt = 0;
  let readCost = 0;
  let changedAt = 0;
  let lastOutcome: string | null = null;
  const noteRead = (sessionId: string, outcome: string, started: number) => {
    readAt = performance.now();
    readCost = readAt - started;
    const keyed = `${sessionId}\0${outcome}`;
    if (keyed !== lastOutcome) changedAt = readAt;
    lastOutcome = keyed;
  };
  const quiet = () => readAt - changedAt >= QUIET_AFTER_MS;
  /* A full read of a long session can take over a second, and nothing tells
     the app which session a store change touched. So the next read waits at
     least as long as the last one took, and longer once reads have found the
     session unchanged for a minute. */
  const sessionReadGapMs = () => {
    const base = timingConfig().activeSessionPollMs;
    if (base === null) return null;
    return Math.max(readCost, quiet() ? QUIET_READ_GAP_MS : base);
  };
  const setSession = (next: Parameters<typeof writeSession>[1]) =>
    writeSession(sessionStore, next);
  const setSessionLoad = (load: SessionLoadState) =>
    writeSessionLoad(sessionStore, load);

  async function refreshSession(
    nextSessionId: string | null,
    nodeDidOverride?: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    const currentRefresh = refreshSeq + 1;
    refreshSeq = currentRefresh;
    if (!nextSessionId) {
      setSession(null);
      setSessionLoad(IDLE_LOAD);
      return null;
    }
    const nodeDid =
      nodeDidOverride === undefined ? store.getState().nodeDid : nodeDidOverride;
    const previous = sessionStore.getState().load;
    const sameSelection =
      previous.sessionId === nextSessionId && previous.nodeDid === nodeDid;
    setSessionLoad({
      phase: sameSelection && previous.phase === "pending" ? "pending" : "loading",
      sessionId: nextSessionId,
      nodeDid,
      found: null,
      error: sameSelection ? previous.error : null,
    });
    const started = performance.now();
    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        nodeDid,
        trackedRequestId(),
        { limit: SESSION_TIMELINE_PAGE_SIZE },
      );
      const stillCurrent =
        acceptsAsyncResult(refreshSeq, currentRefresh) &&
        store.getState().sessionId === nextSessionId &&
        (!nodeDid || store.getState().nodeDid === nodeDid) &&
        (!next || next.sessionId === nextSessionId);
      if (!stillCurrent) return null;
      setSession((current) => (next ? mergeSessionTipSnapshot(current, next) : null));
      noteRead(nextSessionId, next ? sessionSignature(next) : "missing", started);
      setSessionLoad({
        phase: "loaded",
        sessionId: nextSessionId,
        nodeDid,
        found: next !== null,
        error: null,
      });
      return next;
    } catch (error) {
      if (refreshSeq === currentRefresh) {
        const message = String(error);
        const pending =
          error instanceof BridgeInvokeError && error.code === "projectionPending";
        noteRead(nextSessionId, pending ? "pending" : `failed ${message}`, started);
        setSessionLoad({
          phase: pending ? "pending" : "failed",
          sessionId: nextSessionId,
          nodeDid,
          found: null,
          error: message,
        });
      }
      return null;
    }
  }

  async function retrySessionHydration(
    nextSessionId: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    if (!nextSessionId) return null;
    const capturedRun = runEpoch;
    const selected = store.getState();
    const stillCurrent = () =>
      capturedRun === runEpoch &&
      store.getState().nodeDid === selected.nodeDid &&
      store.getState().sessionId === selected.sessionId;
    const projected = readSession(sessionStore);
    const nodeDid =
      projected?.sessionId === nextSessionId
        ? (projected.nodeDid ?? store.getState().nodeDid)
        : null;
    try {
      setError(null);
      await api.retrySessionHydration(nextSessionId, nodeDid);
      if (!stillCurrent()) return null;
      return await refreshSession(nextSessionId, nodeDid);
    } catch (error) {
      if (!stillCurrent()) return null;
      setError(String(error));
      return null;
    }
  }

  /** false when the session is due a full read. A live read stands in for
      one until the next periodic read; otherwise an update reads at once,
      though not back to back, unless the session has gone quiet. */
  async function refreshSessionLiveDelta(): Promise<boolean> {
    const gap = sessionReadGapMs();
    const since = performance.now() - readAt;
    if (gap !== null && since >= gap) return false;
    const current = readSession(sessionStore);
    const requestId = trackedRequestId();
    const request =
      current && requestId && api.fetchSessionLiveDelta
        ? sessionLiveDeltaRequest(current, requestId)
        : null;
    if (!current || !requestId || !request || !api.fetchSessionLiveDelta)
      return gap !== null && (quiet() || since < readCost);
    const capturedRefresh = refreshSeq;
    const capturedLive = ++liveSeq;
    const capturedNodeDid = store.getState().nodeDid;
    const stillCurrent = () =>
      capturedRefresh === refreshSeq &&
      capturedLive === liveSeq &&
      store.getState().nodeDid === capturedNodeDid &&
      store.getState().sessionId === current.sessionId &&
      trackedRequestId() === requestId;
    try {
      const delta = await api.fetchSessionLiveDelta(request);
      if (!stillCurrent()) return true;
      if (!delta) return false;
      const latest = readSession(sessionStore);
      if (!latest || latest.sessionId !== current.sessionId) return true;
      const next = applySessionLiveDelta(latest, delta);
      if (!next) return false;
      setSession(next);
      return true;
    } catch {
      if (!stillCurrent()) return true;
      return false;
    }
  }

  /* a stretch of the open session's rows by message number; a result that
     arrives after the reader moved to another session or node is dropped */
  async function loadSessionRows(
    window: SessionRowsWindow,
    limit = SESSION_TIMELINE_PAGE_SIZE,
  ): Promise<SessionRowsView | null> {
    const capturedRun = runEpoch;
    const current = readSession(sessionStore);
    if (!current) return null;
    const nodeDid = current.nodeDid ?? store.getState().nodeDid;
    const rows = await api.fetchSessionRows({
      sessionId: current.sessionId,
      nodeDid,
      window,
      limit,
    });
    if (
      capturedRun !== runEpoch ||
      store.getState().sessionId !== current.sessionId ||
      (store.getState().nodeDid ?? nodeDid) !== nodeDid
    )
      return null;
    return rows;
  }

  /* every row of a session, open or not, oldest first */
  async function readTranscript(
    nodeDid: string,
    sessionId: string,
  ): Promise<RenderedTimelineItem[]> {
    const pages: RenderedTimelineItem[][] = [];
    let window: SessionRowsWindow = { kind: "tip" };
    for (;;) {
      const page = await api.fetchSessionRows({
        sessionId,
        nodeDid,
        window,
        limit: TRANSCRIPT_READ_SIZE,
      });
      if (!page) break;
      if (page.unreadable) throw new Error(page.unreadable);
      pages.unshift(page.rows);
      const from = page.coveredFrom;
      if (!page.hasOlder || from === null) break;
      if (window.kind === "before" && from >= window.sequence) break;
      window = { kind: "before", sequence: from };
    }
    const seen = new Set<string>();
    return pages.flat().filter((row) => {
      if (seen.has(row.itemKey)) return false;
      seen.add(row.itemKey);
      return true;
    });
  }

  return {
    loadSessionContextDetails,
    readTranscript,
    invalidateSessionReads() {
      runEpoch += 1;
      refreshSeq += 1;
      liveSeq += 1;
    },
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    sessionReadGapMs,
    loadSessionRows,
  };
}
