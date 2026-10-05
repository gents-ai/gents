import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { createClientStore, type ClientStore } from "../src/hooks/clientStore";
import {
  applyFleetSnapshot,
  createFleetStore,
  type FleetStore,
} from "../src/hooks/fleetStore";

/** A fleet store holding `deployments`, as a shell's fleet. */
export function fleetFor(deployments: unknown[] = []) {
  const fleet = createFleetStore();
  applyFleetSnapshot(fleet, {
    bootstrap: {},
    client: { deployments },
  } as unknown as DesktopClientSnapshot);
  return fleet;
}

/* one set of client stores per test, found by the test's api object, since
   a hook's options are rebuilt on every render */
const lifecycleStores = new WeakMap<
  object,
  { client: ClientStore; fleet: FleetStore }
>();
export function storesFor(api: object) {
  const found = lifecycleStores.get(api);
  if (found) return found;
  const stores = { client: createClientStore(), fleet: createFleetStore() };
  lifecycleStores.set(api, stores);
  return stores;
}
