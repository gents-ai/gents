/* Nodes, in the words core is moving to (gents #1799): a node is an
   installation with its own key, runtime and data; the deployments the
   client snapshot carries are nodes. Every screen that needs a node's
   identity reads it through here, so the field rename from agent_did to
   node_did is one edit. */
import type { DeploymentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { isLocalAgent } from "./firstRun";

export type NodeDid = string;

export const nodeDidOf = (node: DeploymentView): NodeDid => node.agentDid;
export const nodeOfSession = (session: SessionSummary): NodeDid => session.agentDid;

/* The working node is the one this machine runs. Its configuration is
   what the sidebar always shows and what a new session is created on. A
   client paired only to remote nodes has none.

   Since the runtime became a user service the desktop pairs with it by
   enrollment, so its record says "enrollment" like any remote peer, and
   only the home's agent DID (bootstrap.initAgentDid) tells them apart;
   isLocalAgent owns that test. */
export const isWorkingNode = (
  node: DeploymentView,
  homeDid: string | null | undefined,
) => isLocalAgent(node, homeDid);
export const workingNode = (
  nodes: readonly DeploymentView[],
  homeDid: string | null | undefined,
): DeploymentView | null => nodes.find((n) => isWorkingNode(n, homeDid)) ?? null;
