import { useMemo } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { interruptVisible } from "../screens/activity-status";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";

import {
  firstNode,
  nodeKeyOf,
  nodeOrFirst,
  sessionKeyOf,
  type NodeView,
} from "../../hooks/fleetStore";
import { usePinnedSessions } from "@/preferences";

import { folderOf, recentFolders } from "../../hooks/chatFolders";
import { useIncompatibleHome } from "../../hooks/useIncompatibleHome";
import { useApp, useView } from "../app/AppContext";
import { isWorkingNode, workingNode } from "../lib/nodes";
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

export function useNodeReach(node: NodeView) {
  const homeDid = useHomeDid();
  const own = useToolAuthority();
  const local = isWorkingNode(node, homeDid);
  const env =
    node.agentEnvironments.find((e) => e.isDefault) ??
    node.agentEnvironments[0] ??
    null;
  return {
    local,
    root: (local ? own.root : env?.workspaceRoot) ?? null,
    ceiling: (local ? own.ceiling : null) ?? null,
    env,
  };
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

/** The newest sessions in scope that aren't pinned or archived; pinned ones
    have their own group. */
export function useRecentSessions(limit: number) {
  const pinned = usePinnedSessions();
  return useInScope((ctx) =>
    recentInScope(defaultScope("recents"), ctx, Infinity)
      .filter((s) => s.archivedAt === null && !pinned.includes(sessionKeyOf(s)))
      .slice(0, limit),
  );
}

/** Pinned sessions in scope, in the order they were pinned. */
export function usePinnedSessionList() {
  const pinned = usePinnedSessions();
  return useInScope((ctx) => {
    const byKey = new Map(
      recentInScope(defaultScope("recents"), ctx, Infinity)
        .filter((s) => s.archivedAt === null)
        .map((s) => [sessionKeyOf(s), s]),
    );
    return pinned.flatMap((key) => byKey.get(key) ?? []);
  });
}

export function useSelectedAgentId() {
  return useView((view) => view.agentReadiness.agentId);
}

export function useChatFolder() {
  const { stores } = useApp();
  const sessionId = stores.selection.use.sessionId();
  return useStore(stores.chat, (state) => folderOf(state.folders, sessionId));
}

export function useRecentFolders() {
  const { stores } = useApp();
  const folders = useStore(stores.chat, (state) => state.folders);
  return useMemo(() => recentFolders(folders), [folders]);
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
    return interruptVisible(view.shellProjection.workflow.kind);
  });
}
