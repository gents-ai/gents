import { createStore, type StoreApi } from "zustand/vanilla";

import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

import type { ManagedServerWait } from "../lib/managedServerStartup";
import { createSelectors, type WithSelectors } from "./createSelectors";

export type LocalServerOperation =
  "start" | "stop" | "restart" | "autostart" | "ensure" | "restore";

export type LocalServerState = {
  status: ManagedServerStatus | null;
  readFailure: string | null;
  wait: ManagedServerWait | null;
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

export const localServer = {
  read(store: LocalServerStore, status: ManagedServerStatus) {
    store.setState({ status, readFailure: null });
  },
  readFailed(store: LocalServerStore, failure: string) {
    store.setState({ status: null, readFailure: failure });
  },
  waiting(store: LocalServerStore, wait: ManagedServerWait | null) {
    store.setState({ wait });
  },
  operating(store: LocalServerStore, operation: LocalServerOperation | null) {
    store.setState({ operation });
  },
};
