import { createStore, type StoreApi } from "zustand/vanilla";

import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

/** The client as last read: the bootstrap summary and the running client's
    state. Nodes, sessions and mailbox items are read through the fleet
    store, which keeps them by key. */
export type ClientState = { snapshot: DesktopClientSnapshot | null };

export type ClientStore = StoreApi<ClientState>;

export function createClientStore() {
  return createStore<ClientState>(() => ({ snapshot: null }));
}
