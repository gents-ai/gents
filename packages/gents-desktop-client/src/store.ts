import {
  assertCompatibleBridgeContract,
  type DesktopClient,
} from "./client.js";
import type { ClientUpdateEvent, Unlisten } from "./transport.js";
import type { DesktopClientSnapshot } from "./types.js";

export type TimingConfig = {
  refreshDebounceMs: number;
};

export const DEFAULT_TIMING: TimingConfig = {
  refreshDebounceMs: 50,
};

export type DesktopStoreState = {
  generation: number;
  snapshot: DesktopClientSnapshot | null;
  lastError: string | null;
  started: boolean;
};

type Listener = () => void;

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
  let refreshRequested = false;
  let lifecycle: Promise<void> = Promise.resolve();

  function emit() {
    for (const listener of listeners) {
      listener();
    }
  }

  function setState(patch: Partial<DesktopStoreState>) {
    state = { ...state, ...patch };
    emit();
  }

  async function refreshOnce() {
    try {
      const snapshot = await client.clientSnapshot();
      setState({
        snapshot: snapshot as DesktopClientSnapshot,
        generation: state.generation + 1,
        lastError: null,
      });
    } catch (error) {
      setState({
        lastError: error instanceof Error ? error.message : String(error),
      });
    }
  }

  function requestRefresh(): Promise<void> {
    refreshRequested = true;
    if (!refreshInFlight) {
      refreshInFlight = (async () => {
        do {
          refreshRequested = false;
          await refreshOnce();
        } while (refreshRequested);
      })().finally(() => {
        refreshInFlight = null;
        if (refreshRequested) {
          void requestRefresh();
        }
      });
    }
    return refreshInFlight;
  }

  function scheduleRefresh(_reason?: ClientUpdateEvent) {
    if (debounceTimer) {
      clearTimeout(debounceTimer);
    }
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      void requestRefresh();
    }, timing.refreshDebounceMs);
  }

  function serializeLifecycle(operation: () => Promise<void>): Promise<void> {
    const result = lifecycle.then(operation, operation);
    lifecycle = result.catch(() => undefined);
    return result;
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
    start() {
      return serializeLifecycle(async () => {
        if (state.started) {
          return;
        }
        assertCompatibleBridgeContract(await client.bridgeContract());
        await client.clientStart();
        unlisten = await client.transport.listenClientUpdated((event) => {
          scheduleRefresh(event);
        });
        setState({ started: true });
        await requestRefresh();
      });
    },
    stop() {
      return serializeLifecycle(async () => {
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
        }
        setState({ started: false });
      });
    },
    refresh: requestRefresh,
  };
}
