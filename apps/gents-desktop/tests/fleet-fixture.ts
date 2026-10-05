import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { applyFleetSnapshot, createFleetStore } from "../src/hooks/fleetStore";

/** A fleet store holding `deployments`, as a shell's fleet. */
export function fleetFor(deployments: unknown[] = []) {
  const fleet = createFleetStore();
  applyFleetSnapshot(fleet, {
    bootstrap: {},
    client: { deployments },
  } as unknown as DesktopClientSnapshot);
  return fleet;
}
