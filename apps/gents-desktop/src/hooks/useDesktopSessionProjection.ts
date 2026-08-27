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
  sessionLiveDeltaRequest,
  SESSION_TIMELINE_PAGE_SIZE,
} from "./desktopShellRuntime";
import {
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
} from "./desktopTimelinePaging";

type SessionProjectionOptions = {
  api: DesktopApiAdapter;
  selectedAgentDidRef: MutableRefObject<string | null>;
  selectedSessionIdRef: MutableRefObject<string | null>;
  selectedTrackedRequestIdRef: MutableRefObject<string | null>;
  setError: (error: string | null) => void;
};

const MAX_HIDDEN_PAGE_HOPS = 8;

/** Own the bounded React page/live overlay and linearize every async commit. */
export function useDesktopSessionProjection({
  api,
  selectedAgentDidRef,
  selectedSessionIdRef,
  selectedTrackedRequestIdRef,
  setError,
}: SessionProjectionOptions) {
  const refreshSeq = useRef(0);
  const sessionRef = useRef<DesktopSessionSnapshot | null>(null);
  const [session, setSessionState] = useState<DesktopSessionSnapshot | null>(null);

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
  ): Promise<DesktopSessionSnapshot | null> {
    const currentRefresh = refreshSeq.current + 1;
    refreshSeq.current = currentRefresh;
    if (!nextSessionId) {
      if (refreshSeq.current === currentRefresh) setSession(null);
      return null;
    }
    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        selectedAgentDidRef.current,
        selectedTrackedRequestIdRef.current,
        { limit: SESSION_TIMELINE_PAGE_SIZE },
      );
      const stillCurrent =
        refreshSeq.current === currentRefresh &&
        selectedSessionIdRef.current === nextSessionId &&
        (!next || next.sessionId === nextSessionId);
      if (!stillCurrent) return null;
      setSession((current) => (next ? mergeSessionTipSnapshot(current, next) : null));
      return next;
    } catch (error) {
      if (refreshSeq.current === currentRefresh) setError(String(error));
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
      if (!delta || selectedSessionIdRef.current !== current.sessionId) return false;
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
          current.agentDid ?? selectedAgentDidRef.current,
          selectedTrackedRequestIdRef.current,
          { limit: SESSION_TIMELINE_PAGE_SIZE, beforeItemKey: cursor },
        );
        if (!older || selectedSessionIdRef.current !== current.sessionId) return false;
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
    setSession,
    refreshSession,
    refreshSessionLiveDelta,
    loadOlderSessionTimeline,
  };
}
