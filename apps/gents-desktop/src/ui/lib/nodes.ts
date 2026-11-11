import type { NodeView } from "../../hooks/fleetStore";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { isLocalNode } from "./firstRun";

export type NodeDid = string;

export const nodeDidOf = (node: { nodeDid: string }): NodeDid => node.nodeDid;
export const nodeOfSession = (session: SessionSummary): NodeDid => session.nodeDid;

export type NodeLike = Pick<NodeView, "nodeDid" | "source">;
export const isWorkingNode = (node: NodeLike, homeDid: string | null | undefined) =>
  isLocalNode(node, homeDid);
/** A joined node's transport peer ID, or null: only an enrollment record
    carries the node's own peer ID, and this computer's node's record holds
    an id of the desktop's making. */
export const joinedPeerId = (
  node: NodeLike & Pick<NodeView, "peerId">,
  homeDid: string | null | undefined,
): string | null =>
  node.source === "enrollment" && !isLocalNode(node, homeDid) && node.peerId
    ? node.peerId
    : null;

export const workingNode = <N extends NodeLike>(
  nodes: readonly N[],
  homeDid: string | null | undefined,
): N | null => nodes.find((n) => isWorkingNode(n, homeDid)) ?? null;
