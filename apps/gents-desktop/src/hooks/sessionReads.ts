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
import {
  isSessionDeliveryPhase,
  selectionMatches,
  sessionDeliveryPhase,
  type SessionLoadState,
} from "../lib/loadingStatus";
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
/** the least wait between full reads of a session that has gone quiet */
const QUIET_READ_GAP_MS = 8_000;
/** The re-reads a waiting phase is given, backing off. A selected request
    that has not reached its owner, or a message still in delivery, may see
    no store notice at all: the runtime holds no replica here. Once these are
    spent the status offers its action instead (Lean
    `ClientSessionLoad.rereadBudget`). A re-read runs only while an observer
    follows the reads: it is requested on that observer's drain loop. */
const WAITING_REREAD_MS = [1_000, 2_000, 4_000, 8_000, 15_000] as const;

/** the delay before the next re-read of a waiting phase that has used
    `used` re-reads, or null once they are spent */
export function waitingRereadDelay(used: number): number | null {
  return WAITING_REREAD_MS[used] ?? null;
}

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
  /* the scheduled re-read of a waiting phase, and the re-reads its streak has
     used; any read the timer did not start begins a new streak */
  let waitingTimer: ReturnType<typeof setTimeout> | undefined;
  let waitingRereads = 0;
  /* the observer's request for a re-read, while one follows these reads */
  let requestReread: (() => void) | null = null;
  const cancelWaitingReread = () => {
    clearTimeout(waitingTimer);
    waitingTimer = undefined;
  };
  const noteRead = (sessionId: string, outcome: string, started: number) => {
    readAt = performance.now();
    readCost = readAt - started;
    const keyed = `${sessionId}\0${outcome}`;
    if (keyed !== lastOutcome) changedAt = readAt;
    lastOutcome = keyed;
  };
  const quiet = () => readAt - changedAt >= QUIET_AFTER_MS;
  /* A full read of a long session can take over a second, and nothing tells
     the app which session a store change touched. So an update reads the
     session in full once the gap since the last full read is at least as
     long as that read took, and longer once reads have found the session
     unchanged for a minute. */
  const sessionReadGapMs = () => {
    const base = timingConfig().sessionReadGapMs;
    if (base === null) return null;
    return Math.max(readCost, quiet() ? QUIET_READ_GAP_MS : base);
  };
  const setSession = (next: Parameters<typeof writeSession>[1]) =>
    writeSession(sessionStore, next);
  const setSessionLoad = (load: SessionLoadState) =>
    writeSessionLoad(sessionStore, load);

  function refreshSession(
    nextSessionId: string | null,
    nodeDidOverride?: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    return readSelectedSession(nextSessionId, nodeDidOverride, false);
  }

  /* A waiting outcome schedules the next re-read while its streak has budget
     and an observer follows; the load's `reread` says whether one follows. */
  function scheduleWaitingReread() {
    const delay = waitingRereadDelay(waitingRereads);
    if (delay === null || !requestReread) return false;
    waitingRereads += 1;
    const captured = refreshSeq;
    waitingTimer = setTimeout(() => {
      waitingTimer = undefined;
      /* a newer read owns what follows */
      if (captured === refreshSeq) requestReread?.();
    }, delay);
    return true;
  }

  /* The latest read's selection moved and no read of the new selection has
     started, which happens only while no observer runs: the load it held has
     no read behind it any more. */
  const settleMovedRead = () => {
    const load = sessionStore.getState().load;
    if (load.reread) setSessionLoad({ ...load, reread: false });
  };

  /* the selection the target names: the same session, and its node unless
     either side names none (Lean `ClientSessionLoad.matchesSelection`) */
  const selects = (sessionId: string, nodeDid: string | null) => {
    const selected = store.getState();
    return selectionMatches(selected.sessionId, selected.nodeDid, {
      sessionId,
      nodeDid,
    });
  };

  async function readSelectedSession(
    nextSessionId: string | null,
    nodeDidOverride: string | null | undefined,
    reread: boolean,
  ): Promise<DesktopSessionSnapshot | null> {
    const nodeDid =
      nodeDidOverride === undefined ? store.getState().nodeDid : nodeDidOverride;
    /* a read takes over the selection's load, so only the selection is read
       (Lean `ClientSessionLoad.launch`); a moved selection reads itself */
    if (nextSessionId && !selects(nextSessionId, nodeDid)) return null;
    const currentRefresh = refreshSeq + 1;
    refreshSeq = currentRefresh;
    cancelWaitingReread();
    if (!reread) waitingRereads = 0;
    if (!nextSessionId) {
      setSession(null);
      setSessionLoad(IDLE_LOAD);
      return null;
    }
    const previous = sessionStore.getState().load;
    const sameSelection =
      previous.sessionId === nextSessionId && previous.nodeDid === nodeDid;
    const waiting =
      previous.phase === "pending" || isSessionDeliveryPhase(previous.phase);
    setSessionLoad({
      phase: sameSelection && waiting ? previous.phase : "loading",
      sessionId: nextSessionId,
      nodeDid,
      /* looking again doesn't bring a missing session back until it's found */
      found: sameSelection && previous.found === false ? false : null,
      error: sameSelection ? previous.error : null,
      reread: true,
    });
    const started = performance.now();
    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        nodeDid,
        trackedRequestId(),
        { limit: SESSION_TIMELINE_PAGE_SIZE },
      );
      /* a newer read owns the load, or the moved selection has its own read
         queued (Lean `ClientSessionLoad.moved_read_leaves_read_queued`) */
      if (!acceptsAsyncResult(refreshSeq, currentRefresh)) return null;
      if (!selects(nextSessionId, nodeDid)) {
        settleMovedRead();
        return null;
      }
      /* A reply for another session cannot be shown, and asking again asks
         the same question, so it fails visibly. */
      if (next && next.sessionId !== nextSessionId) {
        noteRead(nextSessionId, "mismatch", started);
        setSessionLoad({
          phase: "failed",
          sessionId: nextSessionId,
          nodeDid,
          found: null,
          error: "The runtime's answer did not match the selected session.",
          reread: false,
        });
        return null;
      }
      waitingRereads = 0;
      setSession((current) => (next ? mergeSessionTipSnapshot(current, next) : null));
      noteRead(nextSessionId, next ? sessionSignature(next) : "missing", started);
      setSessionLoad({
        phase: "loaded",
        sessionId: nextSessionId,
        nodeDid,
        found: next !== null,
        error: null,
        reread: false,
      });
      return next;
    } catch (error) {
      if (refreshSeq === currentRefresh && !selects(nextSessionId, nodeDid))
        settleMovedRead();
      else if (refreshSeq === currentRefresh) {
        const code = error instanceof BridgeInvokeError ? error.code : null;
        if (code === "permissionDenied") setSession(null);
        const delivery = code ? sessionDeliveryPhase(code) : null;
        const phase = code === "projectionPending" ? "pending" : (delivery ?? "failed");
        const message =
          delivery && error instanceof BridgeInvokeError
            ? error.message
            : String(error);
        noteRead(
          nextSessionId,
          phase === "failed" ? `failed ${message}` : phase,
          started,
        );
        const waiting = phase === "pending" || isSessionDeliveryPhase(phase);
        setSessionLoad({
          phase,
          sessionId: nextSessionId,
          nodeDid,
          found: code === "notFound" ? false : null,
          error: message,
          reread: waiting && scheduleWaitingReread(),
        });
      }
      return null;
    }
  }

  async function reloadSession(
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
      await api.reloadSession(nextSessionId, nodeDid);
      if (!stillCurrent()) return null;
      return await refreshSession(nextSessionId, nodeDid);
    } catch (error) {
      if (!stillCurrent()) return null;
      setError(String(error));
      return null;
    }
  }

  /** Reads a store update for the open session and resolves to when it is
      owed a full read: 0 for now, the ms until the gap ends, or null once a
      read started after the update covers it. Inside the gap a live read
      stands in, but it covers only the live text and turn state, so the
      update is still owed the full read when the gap ends. Without a live
      read the full read waits, not back to back and longer once the session
      has gone quiet. It is never dropped: updates are the only thing that
      reads an open session. */
  async function readSessionUpdate(): Promise<number | null> {
    const gap = sessionReadGapMs();
    const since = performance.now() - readAt;
    if (gap !== null && since >= gap) return 0;
    const current = readSession(sessionStore);
    const requestId = trackedRequestId();
    const request =
      current && requestId && api.fetchSessionLiveDelta
        ? sessionLiveDeltaRequest(current, requestId)
        : null;
    if (!current || !requestId || !request || !api.fetchSessionLiveDelta) {
      if (gap === null) return 0;
      return Math.max(0, (quiet() ? gap : readCost) - since);
    }
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
      if (!stillCurrent()) return null;
      if (!delta) return 0;
      const latest = readSession(sessionStore);
      if (!latest || latest.sessionId !== current.sessionId) return null;
      const next = applySessionLiveDelta(latest, delta);
      if (!next) return 0;
      setSession(next);
      return gap === null ? null : Math.max(0, gap - (performance.now() - readAt));
    } catch {
      if (!stillCurrent()) return null;
      return 0;
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
    let rows: SessionRowsView | null;
    try {
      rows = await api.fetchSessionRows({
        sessionId: current.sessionId,
        nodeDid,
        window,
        limit,
      });
    } catch (error) {
      if (
        error instanceof BridgeInvokeError &&
        error.code === "permissionDenied" &&
        capturedRun === runEpoch &&
        store.getState().sessionId === current.sessionId &&
        (store.getState().nodeDid ?? nodeDid) === nodeDid
      ) {
        setSession(null);
        setSessionLoad({
          phase: "failed",
          sessionId: current.sessionId,
          nodeDid,
          found: null,
          error: error.message,
          reread: false,
        });
      }
      throw error;
    }
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
    /** Requests each waiting phase's re-read through `request` until the
        returned function stops following. */
    followSessionReads(request: () => void) {
      requestReread = request;
      return () => {
        if (requestReread === request) requestReread = null;
      };
    },
    /** The observer stopped: its reads are dropped and no re-read follows, so
        the held load names its next action rather than a read that will not
        come (Lean `ClientSessionLoad.stop_leaves_action`). */
    invalidateSessionReads() {
      cancelWaitingReread();
      runEpoch += 1;
      refreshSeq += 1;
      liveSeq += 1;
      const load = sessionStore.getState().load;
      if (load.phase === "loading")
        setSessionLoad({
          ...load,
          phase: "failed",
          error:
            load.error ?? "The desktop client stopped before this session was read.",
          reread: false,
        });
      else if (load.reread) setSessionLoad({ ...load, reread: false });
    },
    refreshSession,
    /** a waiting phase's re-read, which continues its streak */
    rereadSession: (sessionId: string | null) =>
      readSelectedSession(sessionId, undefined, true),
    reloadSession,
    readSessionUpdate,
    loadSessionRows,
  };
}
