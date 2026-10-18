import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopClientSnapshot,
  ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import type { DesktopStartupPhase } from "../lib/loadingStatus";
import { createSelectors, type WithSelectors } from "./createSelectors";
import type { ManagedServerStartupError } from "../lib/managedServerStartup";

export type ClientState = {
  snapshot: DesktopClientSnapshot | null;
  error: string | null;
  startupPhase: DesktopStartupPhase;
  starting: boolean;
  stopping: boolean;
  managedServerFailure: ManagedServerStartupError | null;
  startupDiagnosticsHint: string | null;
  home: {
    report: ManagedServerResetResult | null;
    busy: boolean;
    generation: number;
    fullReset: boolean;
  };
};

export type ClientStore = WithSelectors<StoreApi<ClientState>>;

export const clientRunning = (state: Pick<ClientState, "snapshot">) =>
  Boolean(state.snapshot?.client);

export const syncHealthOf = (state: Pick<ClientState, "snapshot">) =>
  state.snapshot?.client?.syncHealth ?? null;

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
      managedServerFailure: null,
      startupDiagnosticsHint: null,
      home: { report: null, busy: false, generation: 0, fullReset: false },
    })),
  );
}

export const clientStatus = {
  setError(store: ClientStore, error: string | null) {
    if (store.getState().error !== error) store.setState({ error });
  },
  setStarting(store: ClientStore, starting: boolean) {
    if (store.getState().starting !== starting) store.setState({ starting });
  },
  setDiagnosticsHint(store: ClientStore, hint: string | null) {
    store.setState({ startupDiagnosticsHint: hint });
  },
};
