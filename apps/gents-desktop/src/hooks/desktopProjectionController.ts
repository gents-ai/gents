import {
  isTerminalTurnState,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import type { DesktopUpdateRefreshScope } from "./desktopShellRuntime";

const SNAPSHOT = 1 << 0;
const SESSION_DELTA = 1 << 1;
const SESSION = 1 << 2;
const INDEX_AFTER_TERMINAL = 1 << 3;

export type DesktopProjectionController = {
  request: (scope: DesktopUpdateRefreshScope) => Promise<void>;
  dispose: () => void;
};

type DesktopProjectionControllerOptions = {
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

/**
 * The sole owner of shell projection reads.
 *
 * Requests are merged in one microtask/trailing queue. A full session read
 * supersedes a live delta, delta continuity failure promotes to a full bounded
 * DefraDB projection, and terminal session state schedules the fleet/index
 * projection exactly once. React only receives the resulting bounded view.
 */
export function createDesktopProjectionController({
  currentSessionId,
  refreshSnapshot,
  refreshSession,
  refreshSessionLiveDelta,
  onError = () => {},
}: DesktopProjectionControllerOptions): DesktopProjectionController {
  let active: Promise<void> | null = null;
  let pending = 0;
  let disposed = false;

  const enqueue = (work: number) => {
    pending |= work;
    if (pending & SESSION) {
      pending &= ~SESSION_DELTA;
    }
  };

  const drain = async () => {
    while (!disposed && pending !== 0) {
      const work = pending;
      pending = 0;
      if (work & SNAPSHOT) {
        try {
          await refreshSnapshot();
        } catch (error) {
          onError(error);
        }
      }

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
            // Complete this event's index repair before yielding. Terminal
            // state also tears down the tracked-request effect; queueing the
            // snapshot allowed dispose() to discard the only preview refresh.
            await refreshSnapshot();
          }
        } else if (sessionId && work & SESSION_DELTA) {
          if (!(await refreshSessionLiveDelta())) {
            enqueue(SESSION);
          }
        }
      } catch (error) {
        onError(error);
      }
    }
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
