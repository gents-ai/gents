import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";

export type ConnectNodeState = "not_running" | "older_home" | "outdated" | "serving";
export type ConnectScreen = "loading_settings" | "not_connected";
export type ConnectRefusal =
  | "node_not_running"
  | "older_home"
  | "outdated_node"
  | "enrollment_refused"
  | "approval_pending";

const blocker: Record<ConnectNodeState, ConnectRefusal | null> = {
  not_running: "node_not_running",
  older_home: "older_home",
  outdated: "outdated_node",
  serving: null,
};

/** Lean `DesktopConnect.screen`: an unenrolled desktop is offered the connect
    with what a connect would refuse named up front. */
export function connectScreen(
  node: ConnectNodeState,
  enrolled: boolean,
): [ConnectScreen, ConnectRefusal | null] {
  return enrolled ? ["loading_settings", null] : ["not_connected", blocker[node]];
}

export function connectNodeState(status: ManagedServerStatus): ConnectNodeState {
  if (status.errorCode === "incompatibleLocalStore") return "older_home";
  if (
    status.state === "running" ||
    status.state === "external" ||
    status.state === "starting"
  )
    return "serving";
  return "not_running";
}
