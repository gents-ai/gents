/* Nodes, in the words core is moving to (gents #1799): a node is an
   installation with its own key, runtime and data; the deployments the
   client snapshot carries are nodes. Every screen that needs a node's
   identity reads it through here, so the field rename from agent_did to
   node_did is one edit. */
import type { DeploymentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { isLocalAgent } from "./firstRun";

export type NodeDid = string;

export const nodeDidOf = (node: DeploymentView): NodeDid => node.agentDid;

/* The working node is the one this machine runs. Its configuration is
   what the sidebar always shows and what a new session is created on. A
   client paired only to remote nodes has none.

   Since the runtime became a user service the desktop pairs with it by
   enrollment, so its record says "enrollment" like any remote peer, and
   only the home's agent DID (bootstrap.initAgentDid) tells them apart;
   isLocalAgent owns that test. The app registers the DID once per render,
   before any screen asks, so callers that only hold the nodes can ask. */
let homeDid: string | null = null;
export const setHomeDid = (did: string | null | undefined) => {
  homeDid = did ?? null;
};
export const isWorkingNode = (node: DeploymentView) => isLocalAgent(node, homeDid);
export const workingNode = (nodes: readonly DeploymentView[]): DeploymentView | null =>
  nodes.find(isWorkingNode) ?? null;

/* Lineage across nodes. A worker names the request that spawned it; its
   parent is the session whose latest request that is, on whatever node
   the parent lives, since a background worker may run remotely. */
export const parentOfSession = (
  session: SessionSummary,
  nodes: readonly DeploymentView[],
): SessionSummary | null => {
  /* the bridge names the starting session exactly (agent, session,
     requester), on whatever node lists it */
  const by = session.startedBy;
  return by
    ? (nodes
        .flatMap((n) => n.sessions)
        .find(
          (s) =>
            s.sessionId === by.sessionId &&
            s.agentDid === by.agentDid &&
            s.requesterDid === by.requesterDid,
        ) ?? null)
    : null;
};

/* every worker on any node, by the session that handed it out */
export const workersBySession = (
  nodes: readonly DeploymentView[],
): Map<string, SessionSummary[]> => {
  const out = new Map<string, SessionSummary[]>();
  for (const c of nodes.flatMap((n) => n.sessions)) {
    const parent = parentOfSession(c, nodes);
    if (parent) out.set(parent.sessionId, [...(out.get(parent.sessionId) ?? []), c]);
  }
  return out;
};
