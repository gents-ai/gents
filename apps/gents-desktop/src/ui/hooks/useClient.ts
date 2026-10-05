/* What screens read about the client and the selection, by name. Each
   re-renders its caller only when what it returns changes. */
import { useMemo } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type { DeploymentView } from "@source-inc/gents-desktop-client";

import { folderOf } from "../../hooks/chatFolders";
import { useIncompatibleHome } from "../../hooks/useIncompatibleHome";
import { useApp, useView } from "../app/AppContext";
import type { ScopeContext } from "../lib/scope";
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
  const first = useSelectedDeployment();
  return agentDid ?? first?.agentDid ?? null;
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

/** What a scope is resolved against: the nodes, the selection, the home,
    and each node's sessions and mailbox items by key. */
export function useScopeContext(): ScopeContext {
  const nodes = useDeployments();
  const selectedNodeDid = useSelectedAgentDid();
  const homeDid = useStore(
    useApp().stores.client,
    (state) => state.snapshot?.bootstrap.initAgentDid ?? null,
  );
  const fleet = useFleet(
    useShallow((state) => ({
      sessionsOf: state.sessionsOf,
      mailboxOf: state.mailboxOf,
    })),
  );
  return useMemo(
    () => ({ nodes, selectedNodeDid, homeDid, fleet }),
    [nodes, selectedNodeDid, homeDid, fleet],
  );
}

/** Whether a turn is running that Stop can interrupt. */
export function useInterruptVisible() {
  return useView((view) => {
    const kind = view.shellProjection.workflow.kind;
    return kind === "awaitingObservation" || kind === "turnInProgress";
  });
}
