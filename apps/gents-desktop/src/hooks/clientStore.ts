import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopClientSnapshot,
  ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import type { DesktopStartupPhase } from "../lib/loadingStatus";
import { createSelectors, type WithSelectors } from "./createSelectors";
import type {
  ManagedServerStartupError,
  ManagedServerWait,
} from "../lib/managedServerStartup";

/** The client and its startup: the last read (nodes, sessions and mailbox
    items are read by key through the fleet store), where startup has got
    to, and what the managed server is doing. */
export type ClientState = {
  snapshot: DesktopClientSnapshot | null;
  /** the client's own failure, shown in the banner with Reconnect */
  error: string | null;
  startupPhase: DesktopStartupPhase;
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

export type ClientStore = WithSelectors<StoreApi<ClientState>>;

export function createClientStore(
  startupPhase: DesktopStartupPhase = "loading-configuration",
) {
  return createSelectors(
    createStore<ClientState>(() => ({
      snapshot: null,
      error: null,
      startupPhase,
      starting: false,
      stopping: false,
      managedServerWait: null,
      managedServerFailure: null,
      startupDiagnosticsHint: null,
      home: { report: null, busy: false, generation: 0 },
    })),
  );
}

/** The client's changes made outside its lifecycle, which owns the rest. */
export const clientStatus = {
  /** the client's own failure, shown in the banner; null once it no longer applies */
  setError(store: ClientStore, error: string | null) {
    if (store.getState().error !== error) store.setState({ error });
  },
  /** a start of the client is under way, by the lifecycle or a local runtime's setup */
  setStarting(store: ClientStore, starting: boolean) {
    if (store.getState().starting !== starting) store.setState({ starting });
  },
  /** where the logs are, read before the first snapshot when startup failed */
  setDiagnosticsHint(store: ClientStore, hint: string | null) {
    store.setState({ startupDiagnosticsHint: hint });
  },
};
