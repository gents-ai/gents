import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { applySessionLiveDelta, sessionLiveDeltaRequest } from "./liveDelta";
import { acceptsAsyncResult } from "./observationOrdering";
import { timingConfig } from "./timing";
import {
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
} from "./timelinePaging";
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
    /* a failed read stays said while the same session is read again */
    const previous = sessionStore.getState().load;
    setSessionLoad({
      phase: "loading",
      sessionId: nextSessionId,
      nodeDid,
      found: null,
      error:
        previous.sessionId === nextSessionId && previous.nodeDid === nodeDid
          ? previous.error
          : null,
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
          phase: "failed",
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
    // A delta covers only the live overlay. Enforce history reconciliation at
    // this shared event/poll entry so a continuous wake stream cannot starve
    // it; a null period configures no periodic reconciliation.
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
      // The authoritative full read owns failure reporting. A transient delta
      // failure must not leave a global error after that read recovers.
      return false;
    }
  }

  async function loadOlderSessionTimeline(): Promise<boolean> {
    const capturedRun = runEpoch;
    const capturedNodeDid = store.getState().nodeDid;
    try {
      for (let hop = 0; hop < MAX_HIDDEN_PAGE_HOPS; hop += 1) {
        const current = readSession(sessionStore);
        const cursor = current?.timelinePage?.oldestItemKey ?? null;
        if (!current || !current.timelinePage?.hasOlder || !cursor) return false;
        const older = await api.fetchSessionSnapshot(
          current.sessionId,
          current.nodeDid ?? store.getState().nodeDid,
          trackedRequestId(),
          { limit: SESSION_TIMELINE_PAGE_SIZE, beforeItemKey: cursor },
        );
        if (
          capturedRun !== runEpoch ||
          capturedNodeDid !== store.getState().nodeDid ||
          !older ||
          store.getState().sessionId !== current.sessionId
        )
          return false;
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
      if (capturedRun !== runEpoch) return false;
      setError(String(error));
      return false;
    }
  }

  return {
    /** Revoke pending reads when client observation stops. */
    invalidateSessionReads() {
      runEpoch += 1;
      refreshSeq += 1;
      liveSeq += 1;
    },
    /**
     * Reads a session (the selected node's unless another is given) and holds
     * it, or clears the held session for null. Only the latest read commits,
     * and only while the selection still asks for that session; a failed read
     * stays shown while the same session is read again. Its load state is the
     * session store's.
     */
    refreshSession,
    /**
     * Asks the bridge to hydrate a session again, then reads it. A failure is
     * the client's own state, shown in the banner.
     */
    retrySessionHydration,
    /**
     * Applies the tracked request's live changes to the held session. Returns
     * false when there is nothing to follow or the delta does not apply, so
     * the caller reads the session whole instead.
     */
    refreshSessionLiveDelta,
    /**
     * Loads the held session's next older page, crossing at most a few pages
     * that add no visible row. Returns whether rows were added; the durable
     * cursor is kept at every page, so a later ask resumes there.
     */
    loadOlderSessionTimeline,
  };
}
