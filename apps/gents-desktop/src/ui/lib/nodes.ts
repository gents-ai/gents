import type { NodeView } from "../../hooks/fleetStore";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { isLocalNode } from "./firstRun";

export type NodeDid = string;

export const nodeDidOf = (node: { nodeDid: string }): NodeDid => node.nodeDid;
export const nodeOfSession = (session: SessionSummary): NodeDid => session.nodeDid;

export type NodeLike = Pick<NodeView, "nodeDid" | "source">;
export const isWorkingNode = (node: NodeLike, homeDid: string | null | undefined) =>
  isLocalNode(node, homeDid);
export const workingNode = <N extends NodeLike>(
  nodes: readonly N[],
  homeDid: string | null | undefined,
): N | null => nodes.find((n) => isWorkingNode(n, homeDid)) ?? null;
