import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  SessionRowsView,
  SessionRowsWindow,
} from "@source-inc/gents-desktop-client";
import {
  BridgeInvokeError,
  applySessionLiveDelta,
  sessionLiveDeltaRequest,
} from "@source-inc/gents-desktop-client";
import { acceptsAsyncResult } from "./observationOrdering";
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
  let refreshSeq = 0;
  let liveSeq = 0;
  let runEpoch = 0;
  let reconciledAt = 0;
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
      reconciledAt = performance.now();
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
        setSessionLoad({
          phase:
            error instanceof BridgeInvokeError && error.code === "projectionPending"
              ? "pending"
              : "failed",
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

  async function refreshSessionLiveDelta(): Promise<boolean> {
    const current = readSession(sessionStore);
    const requestId = trackedRequestId();
    if (!current || !requestId || !api.fetchSessionLiveDelta) return false;
    const reconcileMs = timingConfig().activeSessionPollMs;
    if (reconcileMs !== null && performance.now() - reconciledAt >= reconcileMs)
      return false;
    const request = sessionLiveDeltaRequest(current, requestId);
    if (!request) return false;
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

  return {
    invalidateSessionReads() {
      runEpoch += 1;
      refreshSeq += 1;
      liveSeq += 1;
    },
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    loadSessionRows,
  };
}
