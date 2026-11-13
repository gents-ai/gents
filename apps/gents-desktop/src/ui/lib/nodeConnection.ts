import {
  projectRuntimeConnectionStatus,
  runtimeConnectionBlocksAdmission,
  type OperationalStatus,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../hooks/fleetStore";

export function connectionLine(name: string, status: OperationalStatus) {
  switch (status.reason) {
    case "connected":
      return `Connected to ${name}`;
    case "connecting":
      return `Connecting to ${name}`;
    case "disconnected":
      return `Can’t reach ${name}`;
    case "refused":
      return `${name} doesn’t accept this app`;
    case "no_endpoint":
      return `${name} has no address`;
    default:
      return `${name} · ${status.shortLabel}`;
  }
}

export type PageConnection = {
  kind: "loading" | "unavailable" | "stale";
  name: string;
  status: OperationalStatus;
};

export function pageConnection(
  node: NodeView | null,
  hasData: boolean,
): PageConnection | null {
  if (!node || !runtimeConnectionBlocksAdmission(node.runtimeConnection)) return null;
  const status = projectRuntimeConnectionStatus(node.runtimeConnection);
  const name = node.node.displayName ?? node.label;
  if (status.reason === "connecting")
    return hasData ? null : { kind: "loading", name, status };
  return { kind: hasData ? "stale" : "unavailable", name, status };
}
