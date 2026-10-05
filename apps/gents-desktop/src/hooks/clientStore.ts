import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopClientSnapshot,
  ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import type { DesktopStartupPhase } from "../lib/loadingStatus";
import type {
  ManagedServerStartupError,
  ManagedServerWait,
} from "../lib/managedServerStartup";

/** The client and its startup: the last read (nodes, sessions and mailbox
    items are read by key through the fleet store), where startup has got
    to, and what the managed server is doing. */
export type ClientState = {
  snapshot: DesktopClientSnapshot | null;
  startupPhase: DesktopStartupPhase;
  loading: boolean;
  starting: boolean;
  stopping: boolean;
  managedServerWait: ManagedServerWait | null;
  managedServerFailure: ManagedServerStartupError | null;
  /** where the logs are, read before the first snapshot when startup failed */
  startupDiagnosticsHint: string | null;
  /** a home this version cannot open: the reset's report, whether one is in
      progress, and how many times the person started over */
  home: {
    report: ManagedServerResetResult | null;
    busy: boolean;
    generation: number;
  };
};

export type ClientStore = StoreApi<ClientState>;

export function createClientStore(
  startupPhase: DesktopStartupPhase = "loading-configuration",
) {
  return createStore<ClientState>(() => ({
    snapshot: null,
    startupPhase,
    loading: true,
    starting: false,
    stopping: false,
    managedServerWait: null,
    managedServerFailure: null,
    startupDiagnosticsHint: null,
    home: { report: null, busy: false, generation: 0 },
  }));
}

/** A setter for one field. */
export function clientSetter<K extends keyof ClientState>(store: ClientStore, key: K) {
  return (value: ClientState[K]) =>
    store.setState((state) =>
      Object.is(state[key], value) ? state : ({ [key]: value } as Partial<ClientState>),
    );
}
