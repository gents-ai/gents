import { createStore, type StoreApi } from "zustand/vanilla";

import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

import type { ManagedServerWait } from "../lib/managedServerStartup";
import { createSelectors, type WithSelectors } from "./createSelectors";

export type LocalServerOperation =
  "start" | "stop" | "restart" | "autostart" | "ensure" | "restore";

/** The OS-managed local node service as this window last saw it, held once
    for startup, the tray, setup and the node screen. */
export type LocalServerState = {
  /** the service as last read or as the last operation left it */
  status: ManagedServerStatus | null;
  /** why the last read failed, as the bridge said it; null once one succeeds */
  readFailure: string | null;
  /** the wait a start, restart or startup observation is in, while in one */
  wait: ManagedServerWait | null;
  /** the operation under way; they run one at a time */
  operation: LocalServerOperation | null;
};

export type LocalServerStore = WithSelectors<StoreApi<LocalServerState>>;

export function createLocalServerStore() {
  return createSelectors(
    createStore<LocalServerState>(() => ({
      status: null,
      readFailure: null,
      wait: null,
      operation: null,
    })),
  );
}

/** The local server store's changes. */
export const localServer = {
  read(store: LocalServerStore, status: ManagedServerStatus) {
    store.setState({ status, readFailure: null });
  },
  /** A failed read also forgets the last status, which may no longer hold:
      screens say the service could not be checked and offer no control
      that acts on it. */
  readFailed(store: LocalServerStore, failure: string) {
    store.setState({ status: null, readFailure: failure });
  },
  /** null when the wait ends */
  waiting(store: LocalServerStore, wait: ManagedServerWait | null) {
    store.setState({ wait });
  },
  /** null when the operation ends */
  operating(store: LocalServerStore, operation: LocalServerOperation | null) {
    store.setState({ operation });
  },
};
