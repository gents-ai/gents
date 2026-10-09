/* Nodes: an installation with its own key, runtime and data; the deployments
   the client snapshot carries are nodes. Every screen that needs a node's
   identity reads it through here. */
import type { NodeView } from "../../hooks/fleetStore";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { isLocalNode } from "./firstRun";

export type NodeDid = string;

export const nodeDidOf = (node: { nodeDid: string }): NodeDid => node.nodeDid;
export const nodeOfSession = (session: SessionSummary): NodeDid => session.nodeDid;

/* The working node is the one this machine runs. Its configuration is
   what the sidebar always shows and what a new session is created on. A
   client paired only to remote nodes has none.

   Since the runtime became a user service the desktop pairs with it by
   enrollment, so its record says "enrollment" like any remote peer, and
   only the home's node DID (bootstrap.initNodeDid) tells them apart;
   isLocalNode owns that test. */
/** What telling a node apart needs: a deployment, or a node as the fleet
    holds it. */
export type NodeLike = Pick<NodeView, "nodeDid" | "source">;
export const isWorkingNode = (node: NodeLike, homeDid: string | null | undefined) =>
  isLocalNode(node, homeDid);
export const workingNode = <N extends NodeLike>(
  nodes: readonly N[],
  homeDid: string | null | undefined,
): N | null => nodes.find((n) => isWorkingNode(n, homeDid)) ?? null;
