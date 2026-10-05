import {
  useCallback,
  useRef,
  useState,
  type MutableRefObject,
  type SetStateAction,
} from "react";

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

type SessionProjectionOptions = {
  api: DesktopApiAdapter;
  /** the selection, read when a read lands */
  store: SelectionStore;
  selectedTrackedRequestIdRef: MutableRefObject<string | null>;
  setError: (error: string | null) => void;
};

const MAX_HIDDEN_PAGE_HOPS = 8;

/** Own the bounded React page/live overlay and linearize every async commit. */
export function useDesktopSessionProjection({
  api,
  store,
  selectedTrackedRequestIdRef,
  setError,
}: SessionProjectionOptions) {
  const refreshSeq = useRef(0);
  const sessionRef = useRef<DesktopSessionSnapshot | null>(null);
  const [session, setSessionState] = useState<DesktopSessionSnapshot | null>(null);
  const [sessionLoad, setSessionLoad] = useState<SessionLoadState>({
    phase: "idle",
    sessionId: null,
    agentDid: null,
    found: null,
    error: null,
  });

  const setSession = useCallback(
    (next: SetStateAction<DesktopSessionSnapshot | null>) => {
      const resolved = typeof next === "function" ? next(sessionRef.current) : next;
      sessionRef.current = resolved;
      setSessionState(resolved);
    },
    [],
  );

  async function refreshSession(
    nextSessionId: string | null,
    agentDidOverride?: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    const currentRefresh = refreshSeq.current + 1;
    refreshSeq.current = currentRefresh;
    if (!nextSessionId) {
      if (refreshSeq.current === currentRefresh) {
        setSession(null);
        setSessionLoad({
          phase: "idle",
          sessionId: null,
          agentDid: null,
          found: null,
          error: null,
        });
      }
      return null;
    }
    const agentDid =
      agentDidOverride === undefined ? store.getState().agentDid : agentDidOverride;
    setSessionLoad((previous) => ({
      phase: "loading",
      sessionId: nextSessionId,
      agentDid,
      found: null,
      error:
        previous.sessionId === nextSessionId && previous.agentDid === agentDid
          ? previous.error
          : null,
    }));
    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        agentDid,
        selectedTrackedRequestIdRef.current,
        { limit: SESSION_TIMELINE_PAGE_SIZE },
      );
      const stillCurrent =
        acceptsAsyncResult(refreshSeq.current, currentRefresh) &&
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
      if (refreshSeq.current === currentRefresh) {
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
    const projected = sessionRef.current;
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
    const current = sessionRef.current;
    const requestId = selectedTrackedRequestIdRef.current;
    if (!current || !requestId || !api.fetchSessionLiveDelta) return false;
    const request = sessionLiveDeltaRequest(current, requestId);
    if (!request) return false;
    try {
      const delta = await api.fetchSessionLiveDelta(request);
      if (!delta || store.getState().sessionId !== current.sessionId) return false;
      const latest = sessionRef.current;
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
        const current = sessionRef.current;
        const cursor = current?.timelinePage?.oldestItemKey ?? null;
        if (!current || !current.timelinePage?.hasOlder || !cursor) return false;
        const older = await api.fetchSessionSnapshot(
          current.sessionId,
          current.agentDid ?? store.getState().agentDid,
          selectedTrackedRequestIdRef.current,
          { limit: SESSION_TIMELINE_PAGE_SIZE, beforeItemKey: cursor },
        );
        if (!older || store.getState().sessionId !== current.sessionId) return false;
        const previousItemCount = sessionRef.current?.timelineItems.length ?? 0;
        setSession((latest) => mergeOlderSessionTimelinePage(latest, older));
        const next = sessionRef.current;
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
    session,
    sessionLoad,
    setSession,
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    loadOlderSessionTimeline,
  };
}
