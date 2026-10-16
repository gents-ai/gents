import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

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
import { clientRunning, syncHealthOf } from "../../hooks/clientStore";

export function useBootstrap() {
  return useStore(useApp().stores.client, (state) => state.snapshot?.bootstrap);
}

export function useHomeDid() {
  return useStore(
    useApp().stores.client,
    (state) => state.snapshot?.bootstrap.initNodeDid ?? null,
  );
}

export function useSelectedNodeDid(): string | null {
  const nodeDid = useApp().stores.selection.use.nodeDid();
  const first = useFleet((state) => firstNode(state)?.nodeDid ?? null);
  return nodeDid ?? first;
}

export function useSelectedNode(): NodeView | null {
  const nodeDid = useApp().stores.selection.use.nodeDid();
  return useFleet((state) => nodeOrFirst(state, nodeDid));
}

export function useSelectedNodeMailboxItem(
  itemId: string | null | undefined,
): MailboxItemView | null {
  const nodeDid = useApp().stores.selection.use.nodeDid();
  return useFleet((state) => {
    const node = nodeOrFirst(state, nodeDid);
    if (!node || !itemId) return null;
    return state.mailboxOf[nodeKeyOf(node)]?.find((m) => m.itemId === itemId) ?? null;
  });
}

export function useWorkingNode(): NodeView | null {
  const homeDid = useHomeDid();
  return useFleet((state) => workingNode(fleetNodes(state), homeDid));
}

export function useNodeCount() {
  return useFleet((state) => state.nodeKeys.length);
}

export function useOnline() {
  return useStore(useApp().stores.client, clientRunning);
}

export function useEnrollmentRequests() {
  return useStore(
    useApp().stores.client,
    (state) => state.snapshot?.client?.enrollmentRequests,
  );
}

export function useSyncHealth() {
  return useStore(useApp().stores.client, syncHealthOf);
}

export function useToolAuthority() {
  return useStore(
    useApp().stores.client,
    useShallow((state) => ({
      root: state.snapshot?.bootstrap.initToolRoot,
      ceiling: state.snapshot?.bootstrap.initToolCeiling,
    })),
  );
}

export function useInScope<T>(pick: (ctx: ScopeContext) => T): T {
  const selectedNodeDid = useSelectedNodeDid();
  const homeDid = useHomeDid();
  return useFleet(
    useShallow((state) => pick(scopeContextOf(state, selectedNodeDid, homeDid))),
  );
}

export function useMailboxCount() {
  return useInScope((ctx) => mailboxInScope(defaultScope("mailbox"), ctx).length);
}

export function useRecentSessions(limit: number) {
  return useInScope((ctx) => recentInScope(defaultScope("recents"), ctx, limit));
}

export function useSelectedAgentId() {
  return useView((view) => view.agentReadiness.agentId);
}

export function useChatFolder() {
  const { stores } = useApp();
  const sessionId = stores.selection.use.sessionId();
  return useStore(stores.chat, (state) => folderOf(state.folders, sessionId));
}

export function useMailboxCause() {
  const { stores } = useApp();
  const route = stores.selection.use.mailboxRoute();
  const agentId = useSelectedAgentId();
  const sessionId = stores.selection.use.sessionId();
  return route ? { itemId: route.itemId, agentId: agentId ?? "", sessionId } : null;
}

export function useStartup() {
  const { stores, lifecycle, actions } = useApp();
  const managedServerWait = useStore(stores.localServer, (s) => s.wait);
  const state = useStore(
    stores.client,
    useShallow((s) => ({
      phase: s.startupPhase,
      error: s.error,
      managedServerFailure: s.managedServerFailure,
      diagnosticsHint:
        s.snapshot?.bootstrap.diagnosticsHint || s.startupDiagnosticsHint,
    })),
  );
  const failure = state.managedServerFailure;
  return {
    ...state,
    managedServerWait,
    incompatibleHome: useIncompatibleHome(stores.client, lifecycle.home),
    // A restart needs the node name and authority the failed start reported.
    canRestartManagedServer: Boolean(
      failure?.status.nodeName &&
      failure.status.effectiveToolCeiling &&
      actions.localServerOffers.restart,
    ),
  };
}

export function useSessionLoad() {
  return useStore(useApp().stores.session, (state) => state.load);
}

export function useInterruptVisible() {
  return useView((view) => {
    const kind = view.shellProjection.workflow.kind;
    return kind === "awaitingObservation" || kind === "turnInProgress";
  });
}
