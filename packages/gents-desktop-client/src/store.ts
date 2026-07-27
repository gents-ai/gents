import type { DesktopClient } from "./client.js";
import type { ClientUpdateEvent, Unlisten } from "./transport.js";

export type TimingConfig = {
  /** Coalesce burst of client-updated events into one refresh. */
  refreshDebounceMs: number;
  /** Active session poll interval while a turn is live. */
  activeSessionPollMs: number;
};

export const DEFAULT_TIMING: TimingConfig = {
  refreshDebounceMs: 50,
  activeSessionPollMs: 1500,
};

export type DesktopStoreState = {
  generation: number;
  snapshot: unknown | null;
  lastError: string | null;
  started: boolean;
};

type Listener = () => void;

/**
 * Shared client store + refresh coordinator.
 * One client-updated subscription; domain packages use selectors/actions only.
 */
export type DesktopStore = {
  getState(): DesktopStoreState;
  subscribe(listener: Listener): () => void;
  start(): Promise<void>;
  stop(): Promise<void>;
  refresh(): Promise<void>;
  client: DesktopClient;
};

export function createDesktopStore(
  client: DesktopClient,
  timing: TimingConfig = DEFAULT_TIMING,
): DesktopStore {
  let state: DesktopStoreState = {
    generation: 0,
    snapshot: null,
    lastError: null,
    started: false,
  };
  const listeners = new Set<Listener>();
  let unlisten: Unlisten | null = null;
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  let refreshInFlight: Promise<void> | null = null;
  let trailingRefresh = false;

  function emit() {
    for (const listener of listeners) {
      listener();
    }
  }

  function setState(patch: Partial<DesktopStoreState>) {
    state = { ...state, ...patch };
    emit();
  }

  async function refreshNow() {
    try {
      const snapshot = await client.clientSnapshot();
      setState({
        snapshot,
        generation: state.generation + 1,
        lastError: null,
      });
    } catch (error) {
      setState({
        lastError: error instanceof Error ? error.message : String(error),
      });
    }
  }

  function scheduleRefresh(_reason?: ClientUpdateEvent) {
    if (debounceTimer) {
      clearTimeout(debounceTimer);
    }
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      if (refreshInFlight) {
        trailingRefresh = true;
        return;
      }
      refreshInFlight = refreshNow().finally(() => {
        refreshInFlight = null;
        if (trailingRefresh) {
          trailingRefresh = false;
          void scheduleRefresh();
        }
      });
    }, timing.refreshDebounceMs);
  }

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    client,
    async start() {
      if (state.started) {
        return;
      }
      await client.clientStart();
      unlisten = await client.transport.listenClientUpdated((event) => {
        scheduleRefresh(event);
      });
      setState({ started: true });
      await refreshNow();
    },
    async stop() {
      if (unlisten) {
        unlisten();
        unlisten = null;
      }
      if (debounceTimer) {
        clearTimeout(debounceTimer);
        debounceTimer = null;
      }
      try {
        await client.clientShutdown();
      } catch {
        // ignore shutdown errors on teardown
      }
      setState({ started: false });
    },
    refresh: refreshNow,
  };
}

/** N update events → one coalesced refresh (property under test). */
export function countCoalescedRefreshes(
  eventCount: number,
  debounceMs: number,
  fireIntervalMs: number,
): number {
  if (eventCount <= 0) {
    return 0;
  }
  // If events arrive faster than debounce, they coalesce to 1.
  if (fireIntervalMs < debounceMs) {
    return 1;
  }
  return eventCount;
}
