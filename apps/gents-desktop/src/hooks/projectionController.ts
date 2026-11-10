import {
  isTerminalTurnState,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

export type DesktopUpdateRefreshScope =
  "snapshot" | "sessionDelta" | "session" | "sessionEvent" | "full";

/** What the drain loop can be asked for: an update's scope, or a waiting
    phase's re-read, which continues its streak unless another request for
    the session joins it. */
export type ProjectionRequest = DesktopUpdateRefreshScope | "reread";

/** A pairing-health change re-reads a selected session that shows no
    snapshot yet: the pairing is one of the signals its delivery state derives
    from, and a read already in flight may have taken the pairing before the
    change. */
export function desktopUpdateRefreshScope(
  reason: string | undefined,
  selectedSessionId: string | null,
  selectedTrackedRequestId: string | null,
  selectedSessionUnresolved = false,
): DesktopUpdateRefreshScope {
  if (reason === "health")
    return selectedSessionId && selectedSessionUnresolved ? "full" : "snapshot";
  if (selectedSessionId && selectedTrackedRequestId) {
    if (reason === "store") return "sessionDelta";
    return "sessionEvent";
  }
  return "full";
}

const SNAPSHOT = 1 << 0;
const SESSION_DELTA = 1 << 1;
const SESSION = 1 << 2;
const INDEX_AFTER_TERMINAL = 1 << 3;
const REREAD = 1 << 4;

export type ProjectionController = {
  request: (scope: ProjectionRequest) => Promise<void>;
  dispose: () => void;
};

type ProjectionControllerOptions = {
  currentSessionId: () => string | null;
  refreshSnapshot: () => Promise<void>;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  /** a waiting phase's re-read of the session */
  rereadSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  /** reads a store update for the open session; resolves to the ms until
      it is owed a full read (0: now), or null when a read covers it */
  readSessionUpdate: () => Promise<number | null>;
  onError?: (error: unknown) => void;
};

function requestedWork(scope: ProjectionRequest) {
  switch (scope) {
    case "reread":
      return REREAD;
    case "snapshot":
      return SNAPSHOT;
    case "sessionDelta":
      return SESSION_DELTA;
    case "session":
      return SESSION;
    case "sessionEvent":
      return SESSION | INDEX_AFTER_TERMINAL;
    case "full":
      return SNAPSHOT | SESSION;
  }
}

export function createProjectionController({
  currentSessionId,
  refreshSnapshot,
  refreshSession,
  rereadSession,
  readSessionUpdate,
  onError = () => {},
}: ProjectionControllerOptions): ProjectionController {
  let active: Promise<void> | null = null;
  let snapshotActive: Promise<void> | null = null;
  let snapshotPending = false;
  let pending = 0;
  let disposed = false;
  /* the one full read an update not yet read in full is owed; a full read
     that starts after the update pays it */
  let owed: ReturnType<typeof setTimeout> | undefined;
  const settleOwed = () => {
    clearTimeout(owed);
    owed = undefined;
  };

  const enqueue = (work: number) => {
    pending |= work;
    if (pending & (SESSION | REREAD)) {
      pending &= ~SESSION_DELTA;
    }
  };

  const refreshIndex = () => {
    snapshotPending = true;
    if (!snapshotActive) {
      snapshotActive = Promise.resolve()
        .then(async () => {
          while (snapshotPending) {
            snapshotPending = false;
            try {
              await refreshSnapshot();
            } catch (error) {
              onError(error);
            }
          }
        })
        .finally(() => {
          snapshotActive = null;
        });
    }
    return snapshotActive;
  };

  const drain = async () => {
    /* a snapshot refresh is shared while it runs and the next starts only
       after it ends, so the latest is the only one still to wait for; a loop
       kept busy by requests must not hold one per pass */
    let snapshot: Promise<void> | null = null;
    while (!disposed && pending !== 0) {
      const work = pending;
      pending = 0;
      try {
        const sessionId = currentSessionId();
        if (work & (SESSION | REREAD)) {
          settleOwed();
          const next =
            work & SESSION
              ? await refreshSession(sessionId)
              : await rereadSession(sessionId);
          if (
            work & INDEX_AFTER_TERMINAL &&
            !(work & SNAPSHOT) &&
            next?.turnState &&
            isTerminalTurnState(next.turnState)
          ) {
            snapshot = refreshIndex();
          }
          if (!sessionId && !(work & SNAPSHOT)) {
            snapshot = refreshIndex();
          }
        } else if (sessionId && work & SESSION_DELTA) {
          const due = await readSessionUpdate();
          if (due === 0) enqueue(SESSION | INDEX_AFTER_TERMINAL);
          else if (due !== null && owed === undefined && !disposed) {
            owed = setTimeout(() => {
              owed = undefined;
              void controller.request("sessionEvent");
            }, due);
          }
        }
      } catch (error) {
        onError(error);
      }
      if (work & SNAPSHOT) {
        try {
          snapshot = refreshIndex();
        } catch (error) {
          onError(error);
        }
      }
    }
    /* wrapped: an async function returning a promise would wait for it */
    return { snapshot };
  };

  const controller: ProjectionController = {
    request(scope) {
      if (disposed) return Promise.resolve();
      enqueue(requestedWork(scope));
      if (!active) {
        active = Promise.resolve()
          .then(drain)
          .finally(() => {
            active = null;
          })
          .then(async ({ snapshot }) => {
            await snapshot;
          });
      }
      return active;
    },
    dispose() {
      disposed = true;
      pending = 0;
      settleOwed();
    },
  };
  return controller;
}
