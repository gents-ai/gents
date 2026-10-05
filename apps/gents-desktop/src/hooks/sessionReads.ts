import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import {
  applySessionLiveDelta,
  acceptsAsyncResult,
  sessionLiveDeltaRequest,
  SESSION_TIMELINE_PAGE_SIZE,
} from "./desktopShellRuntime";
import {
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
} from "./desktopTimelinePaging";
import type { SessionLoadState } from "../lib/loadingStatus";
import type { SelectionStore } from "./selectionStore";
import {
  IDLE_LOAD,
  readSession,
  writeSession,
  writeSessionLoad,
  type SessionStore,
} from "./sessionStore";

type SessionReadParams = {
  api: DesktopApiAdapter;
  /** the selection, read when a read lands */
  store: SelectionStore;
  /** where the selected session's reads land */
  sessionStore: SessionStore;
  /** the request being tracked now, read when a live read is asked for */
  trackedRequestId: () => string | null;
  setError: (error: string | null) => void;
};

const MAX_HIDDEN_PAGE_HOPS = 8;

/**
 * The selected session's reads: its tip, a hydration retry, the tracked
 * request's live delta and older pages. Each reads the selection and the
 * session store when it runs and commits only while the selection still
 * asks for what it read, so a later read always wins.
 */
export function createSessionReads({
  api,
  store,
  sessionStore,
  trackedRequestId,
  setError,
}: SessionReadParams) {
  let refreshSeq = 0;
  const setSession = (next: Parameters<typeof writeSession>[1]) =>
    writeSession(sessionStore, next);
  const setSessionLoad = (load: SessionLoadState) =>
    writeSessionLoad(sessionStore, load);

  async function refreshSession(
    nextSessionId: string | null,
    agentDidOverride?: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    const currentRefresh = refreshSeq + 1;
    refreshSeq = currentRefresh;
    if (!nextSessionId) {
      setSession(null);
      setSessionLoad(IDLE_LOAD);
      return null;
    }
    const agentDid =
      agentDidOverride === undefined ? store.getState().agentDid : agentDidOverride;
    /* a failed read stays said while the same session is read again */
    const previous = sessionStore.getState().load;
    setSessionLoad({
      phase: "loading",
      sessionId: nextSessionId,
      agentDid,
      found: null,
      error:
        previous.sessionId === nextSessionId && previous.agentDid === agentDid
          ? previous.error
          : null,
    });
    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        agentDid,
        trackedRequestId(),
        { limit: SESSION_TIMELINE_PAGE_SIZE },
      );
      const stillCurrent =
        acceptsAsyncResult(refreshSeq, currentRefresh) &&
        store.getState().sessionId === nextSessionId &&
        (!agentDid || store.getState().agentDid === agentDid) &&
        (!next || next.sessionId === nextSessionId);
      if (!stillCurrent) return null;
      setSession((current) => (next ? mergeSessionTipSnapshot(current, next) : null));
      setSessionLoad({
        phase: "loaded",
        sessionId: nextSessionId,
        agentDid,
        found: next !== null,
        error: null,
      });
      return next;
    } catch (error) {
      if (refreshSeq === currentRefresh) {
        const message = String(error);
        setSessionLoad({
          phase: "failed",
          sessionId: nextSessionId,
          agentDid,
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
    const projected = readSession(sessionStore);
    const agentDid =
      projected?.sessionId === nextSessionId
        ? (projected.agentDid ?? store.getState().agentDid)
        : null;
    try {
      setError(null);
      await api.retrySessionHydration(nextSessionId, agentDid);
      return await refreshSession(nextSessionId, agentDid);
    } catch (error) {
      setError(String(error));
      return null;
    }
  }

  async function refreshSessionLiveDelta(): Promise<boolean> {
    const current = readSession(sessionStore);
    const requestId = trackedRequestId();
    if (!current || !requestId || !api.fetchSessionLiveDelta) return false;
    const request = sessionLiveDeltaRequest(current, requestId);
    if (!request) return false;
    try {
      const delta = await api.fetchSessionLiveDelta(request);
      if (!delta || store.getState().sessionId !== current.sessionId) return false;
      const latest = readSession(sessionStore);
      if (!latest || latest.sessionId !== current.sessionId) return true;
      const next = applySessionLiveDelta(latest, delta);
      if (!next) return false;
      setSession(next);
      return true;
    } catch (error) {
      setError(String(error));
      return false;
    }
  }

  async function loadOlderSessionTimeline(): Promise<boolean> {
    try {
      for (let hop = 0; hop < MAX_HIDDEN_PAGE_HOPS; hop += 1) {
        const current = readSession(sessionStore);
        const cursor = current?.timelinePage?.oldestItemKey ?? null;
        if (!current || !current.timelinePage?.hasOlder || !cursor) return false;
        const older = await api.fetchSessionSnapshot(
          current.sessionId,
          current.agentDid ?? store.getState().agentDid,
          trackedRequestId(),
          { limit: SESSION_TIMELINE_PAGE_SIZE, beforeItemKey: cursor },
        );
        if (!older || store.getState().sessionId !== current.sessionId) return false;
        const previousItemCount = readSession(sessionStore)?.timelineItems.length ?? 0;
        setSession((latest) => mergeOlderSessionTimelinePage(latest, older));
        const next = readSession(sessionStore);
        if ((next?.timelineItems.length ?? 0) > previousItemCount) return true;
        if (
          !next?.timelinePage?.hasOlder ||
          !next.timelinePage.oldestItemKey ||
          next.timelinePage.oldestItemKey === cursor
        ) {
          return false;
        }
      }
      // The durable cursor was committed at every hop. A later user gesture
      // resumes from there without making this interaction unbounded.
      return false;
    } catch (error) {
      setError(String(error));
      return false;
    }
  }

  return {
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    loadOlderSessionTimeline,
  };
}
