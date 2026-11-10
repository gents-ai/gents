import {
  projectRuntimeConnectionStatus,
  runtimeConnectionBlocksAdmission,
  type OperationalStatus,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../hooks/fleetStore";

/** The node's name beside how its runtime connection stands. */
export const connectionLine = (name: string, status: OperationalStatus) =>
  `${name} · ${status.shortLabel}`;

/** Where a page of one node's data says the connection isn't up: in place of
    the page while it connects (`loading`) or while nothing can be shown
    (`unavailable`), or over data a lost connection kept (`stale`). */
export type PageConnection = {
  kind: "loading" | "unavailable" | "stale";
  name: string;
  status: OperationalStatus;
};

/** What a page of one node's data says about the connection, or null when
    the page shows as it is. With nothing to show, the page loads while the
    node connects, or says why it can't, rather than saying the node has
    none. Data a lost connection kept is marked as possibly out of date. */
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
