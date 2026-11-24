import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

import { connectNodeState, connectScreen } from "./connectScreen";

/** What a ready app shows: the shell, or a screen that asks something of the
    person. Waiting is never one of them. */
export type StartupGate = "shell" | "unreadable" | "not_connected";

/** Once this device's own saved settings are read, the shell shows. A first
    run still unsettled because the node's configuration has not been read
    from the node yet waits in the shell, with the node shown connecting:
    that read needs the node, and the node may be slow, stopped or away. Only
    settings that cannot be read, or a node this desktop is not connected to,
    take the window instead, each with what to do about it (Lean
    `ClientSnapshotObservation.startupGate`, with the connect screen's
    verdict on the local node as its `nodeNotConnected`). */
export function projectStartupGate(
  settled: boolean,
  initConfigStatus: "absent" | "configured" | "unreadable" | null | undefined,
  localNode: ManagedServerStatus | null,
): StartupGate {
  if (settled) return "shell";
  if (initConfigStatus === "unreadable") return "unreadable";
  if (
    initConfigStatus === "configured" &&
    localNode &&
    connectScreen(connectNodeState(localNode), localNode.pairingReady)[0] ===
      "not_connected"
  )
    return "not_connected";
  return "shell";
}
