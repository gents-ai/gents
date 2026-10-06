/* What screens read about the client and the selection, by name. Each
   re-renders its caller only when what it returns changes. */
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type { DeploymentView } from "@source-inc/gents-desktop-client";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";

import {
  firstNode,
  nodeKeyOf,
  nodeOrFirst,
  type NodeView,
} from "../../hooks/fleetStore";

import { folderOf } from "../../hooks/chatFolders";
import { useIncompatibleHome } from "../../hooks/useIncompatibleHome";
import { useApp, useView } from "../app/AppContext";
import { workingNode } from "../lib/nodes";
import {
  defaultScope,
  fleetNodes,
  mailboxInScope,
  recentInScope,
  scopeContextOf,
  type ScopeContext,
} from "../lib/scope";
import { useFleet } from "./useFleet";

const NO_DEPLOYMENTS: DeploymentView[] = [];

/** The client as last read. */
export function useSnapshot() {
  return useApp().stores.client.use.snapshot();
}

/** The home's agent DID, which marks the node this machine runs. */
export function useHomeDid() {
  return useStore(
    useApp().stores.client,
    (state) => state.snapshot?.bootstrap.initAgentDid ?? null,
  );
}

/** Every node the client lists, sessions and mailbox included. */
export function useDeployments(): DeploymentView[] {
  return useStore(
    useApp().stores.client,
    (state) => state.snapshot?.client?.deployments ?? NO_DEPLOYMENTS,
  );
}

/** The selected node, or the first one while nothing is selected yet. */
export function useSelectedDeployment(): DeploymentView | null {
  const deployments = useDeployments();
  const agentDid = useApp().stores.selection.use.agentDid();
  return (
    deployments.find((deployment) => deployment.agentDid === agentDid) ??
    deployments[0] ??
    null
  );
}

/** The selected node's DID, or the first node's while nothing is selected. */
export function useSelectedAgentDid(): string | null {
  const agentDid = useApp().stores.selection.use.agentDid();
  const first = useFleet((state) => firstNode(state)?.agentDid ?? null);
  return agentDid ?? first;
}

/** The selected node as the fleet holds it, or the first one while nothing
    is selected yet; the same object while it is unchanged. */
export function useSelectedNode(): NodeView | null {
  const agentDid = useApp().stores.selection.use.agentDid();
  return useFleet((state) => nodeOrFirst(state, agentDid));
}

/** A mailbox item on the selected node, or the first node while nothing is
    selected yet; null when that node no longer lists it. */
export function useSelectedNodeMailboxItem(
  itemId: string | null | undefined,
): MailboxItemView | null {
  const agentDid = useApp().stores.selection.use.agentDid();
  return useFleet((state) => {
    const node = nodeOrFirst(state, agentDid);
    if (!node || !itemId) return null;
    return state.mailboxOf[nodeKeyOf(node)]?.find((m) => m.itemId === itemId) ?? null;
  });
}

/** The node this machine runs, as the fleet holds it; null when the client
    is paired only to remote nodes. */
export function useWorkingNode(): NodeView | null {
  const homeDid = useHomeDid();
  return useFleet((state) => workingNode(fleetNodes(state), homeDid));
}

/** How many nodes the client can see. */
export function useNodeCount() {
  return useFleet((state) => state.nodeKeys.length);
}

/** Whether the client is running. */
export function useOnline() {
  return useStore(useApp().stores.client, (state) => Boolean(state.snapshot?.client));
}

/** How the client's database sync is doing. */
export function useSyncHealth() {
  return useStore(
    useApp().stores.client,
    (state) => state.snapshot?.client?.syncHealth,
  );
}

/** The tool root and ceiling this machine's node was set up with. */
export function useToolAuthority() {
  return useStore(
    useApp().stores.client,
    useShallow((state) => ({
      root: state.snapshot?.bootstrap.initToolRoot,
      ceiling: state.snapshot?.bootstrap.initToolCeiling,
    })),
  );
}

/** A value worked out over the fleet in the selection's scope; re-renders
    when it changes, item by item for a list. */
export function useInScope<T>(pick: (ctx: ScopeContext) => T): T {
  const selectedNodeDid = useSelectedAgentDid();
  const homeDid = useHomeDid();
  return useFleet(
    useShallow((state) => pick(scopeContextOf(state, selectedNodeDid, homeDid))),
  );
}

/** How many open mailbox items wait in the mailbox's default scope. */
export function useMailboxCount() {
  return useInScope((ctx) => mailboxInScope(defaultScope("mailbox"), ctx).length);
}

/** The newest sessions in the recents' default scope. */
export function useRecentSessions(limit: number) {
  return useInScope((ctx) => recentInScope(defaultScope("recents"), ctx, limit));
}

export function useSelectedSessionId() {
  return useApp().stores.selection.use.sessionId();
}

/** The behavior a message goes to: the selection's, settled against the node. */
export function useSelectedBehaviorId() {
  return useView((view) => view.behaviorReadiness.behaviorId);
}

/** The folder the selected chat works in. */
export function useChatFolder() {
  const { stores } = useApp();
  const sessionId = stores.selection.use.sessionId();
  return useStore(stores.chat, (state) => folderOf(state.folders, sessionId));
}

/** The mailbox item the next message answers, while it is held. */
export function useMailboxCause() {
  const { stores } = useApp();
  const route = stores.selection.use.mailboxRoute();
  const behaviorId = useSelectedBehaviorId();
  const sessionId = stores.selection.use.sessionId();
  return route
    ? { itemId: route.itemId, behaviorId: behaviorId ?? "", sessionId }
    : null;
}

/** Startup as screens show it. */
export function useStartup() {
  const { api, stores, lifecycle } = useApp();
  const state = useStore(
    stores.client,
    useShallow((s) => ({
      phase: s.startupPhase,
      error: s.error,
      managedServerWait: s.managedServerWait,
      managedServerFailure: s.managedServerFailure,
      diagnosticsHint:
        s.snapshot?.bootstrap.diagnosticsHint || s.startupDiagnosticsHint,
    })),
  );
  const failure = state.managedServerFailure;
  return {
    ...state,
    incompatibleHome: useIncompatibleHome(stores.client, lifecycle.home),
    /* a restart needs the agent and authority the failed start reported */
    canRestartManagedServer: Boolean(
      failure?.status.agentName &&
      failure.status.effectiveToolCeiling &&
      api.restartManagedServer,
    ),
  };
}

/** How the selected session's last read went. */
export function useSessionLoad() {
  return useStore(useApp().stores.session, (state) => state.load);
}

/** Whether a turn is running that Stop can interrupt. */
export function useInterruptVisible() {
  return useView((view) => {
    const kind = view.shellProjection.workflow.kind;
    return kind === "awaitingObservation" || kind === "turnInProgress";
  });
}
