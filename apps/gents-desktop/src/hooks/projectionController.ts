import {
  isTerminalTurnState,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

export type DesktopUpdateRefreshScope =
  "snapshot" | "sessionDelta" | "session" | "sessionEvent" | "full";

export function desktopUpdateRefreshScope(
  reason: string | undefined,
  selectedSessionId: string | null,
  selectedTrackedRequestId: string | null,
): DesktopUpdateRefreshScope {
  if (reason === "health") return "snapshot";
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

export type ProjectionController = {
  request: (scope: DesktopUpdateRefreshScope) => Promise<void>;
  dispose: () => void;
};

type ProjectionControllerOptions = {
  currentSessionId: () => string | null;
  refreshSnapshot: () => Promise<void>;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  refreshSessionLiveDelta: () => Promise<boolean>;
  onError?: (error: unknown) => void;
};

function requestedWork(scope: DesktopUpdateRefreshScope) {
  switch (scope) {
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
  refreshSessionLiveDelta,
  onError = () => {},
}: ProjectionControllerOptions): ProjectionController {
  let active: Promise<void> | null = null;
  let snapshotActive: Promise<void> | null = null;
  let snapshotPending = false;
  let pending = 0;
  let disposed = false;

  const enqueue = (work: number) => {
    pending |= work;
    if (pending & SESSION) {
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
    const snapshots: Promise<void>[] = [];
    while (!disposed && pending !== 0) {
      const work = pending;
      pending = 0;
      try {
        const sessionId = currentSessionId();
        if (work & SESSION) {
          const next = await refreshSession(sessionId);
          if (
            work & INDEX_AFTER_TERMINAL &&
            !(work & SNAPSHOT) &&
            next?.turnState &&
            isTerminalTurnState(next.turnState)
          ) {
            snapshots.push(refreshIndex());
          }
          if (!sessionId && !(work & SNAPSHOT)) {
            snapshots.push(refreshIndex());
          }
        } else if (sessionId && work & SESSION_DELTA) {
          if (!(await refreshSessionLiveDelta())) {
            enqueue(SESSION | INDEX_AFTER_TERMINAL);
          }
        }
      } catch (error) {
        onError(error);
      }
      if (work & SNAPSHOT) {
        try {
          snapshots.push(refreshIndex());
        } catch (error) {
          onError(error);
        }
      }
    }
    return snapshots;
  };

  return {
    request(scope) {
      if (disposed) return Promise.resolve();
      enqueue(requestedWork(scope));
      if (!active) {
        active = Promise.resolve()
          .then(drain)
          .finally(() => {
            active = null;
          })
          .then(async (snapshots) => {
            await Promise.all(snapshots);
          });
      }
      return active;
    },
    dispose() {
      disposed = true;
      pending = 0;
    },
  };
}
