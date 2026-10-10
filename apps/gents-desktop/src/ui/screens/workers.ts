/* The sessions this one's calls reached. The runtime's lineage owner
   (`gents::session_origin::lineage`) decides which sessions this one started
   — "workers" to the person — and which it only messaged; both are
   ordinary sessions to the runtime. The bridge maps that answer and the
   request each agents-tool call caused; this joins them to the session list
   for who each session is, and to this transcript's rows by the call. Every
   fact here has a field in the bridge contract; where the contract is silent
   the state says so instead of guessing. */
import { useEffect, useMemo, useState } from "react";
import type {
  BackgroundedToolView,
  CausedRequestView,
  DesktopOperationsSnapshot,
  LinkedSessionView,
  RenderedToolCallView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { useShallow } from "zustand/react/shallow";
import { useApp } from "@/app/AppContext";
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";
import { useSessionFacts } from "../hooks/useSelectedSession";

export type Worker = {
  sessionId: string;
  nodeDid: string;
  summary: SessionSummary | null;
  /* the lineage owner's link: this session started it */
  link: LinkedSessionView;
};

/* what one agent_new/agent_message row reached */
export type Reached = {
  /* the request this row's call caused */
  request: CausedRequestView;
  summary: SessionSummary | null;
  /* the worker, when the call reached a session this one started */
  worker: Worker | null;
};

export type Workers = {
  /* the sessions this one started */
  all: Worker[];
  byToolCall: (tool: RenderedToolCallView) => Reached | null;
  /* the operations facts for a background process row */
  background: (tool: RenderedToolCallView) => BackgroundedToolView | null;
  loaded: boolean;
};

export const NO_WORKERS: Workers = {
  all: [],
  byToolCall: () => null,
  background: () => null,
  loaded: false,
};

/* A session is identified by its whole scope (node, label, requester),
   never by its label alone. */
export const scopeKey = (r: {
  nodeDid: string | null;
  sessionId: string | null;
  requesterDid: string | null;
}) => `${r.nodeDid ?? ""}\u0000${r.sessionId ?? ""}\u0000${r.requesterDid ?? ""}`;

export const summariesByScope = (sessions: readonly SessionSummary[] | undefined) =>
  new Map((sessions ?? []).map((s) => [scopeKey(s), s]));

/* The sessions the node with this DID lists under these scopes, and only
   those: a change elsewhere in its list does not re-render the caller. */
export function useListedScopes(
  nodeDid: string | null | undefined,
  scopes: readonly string[],
): readonly SessionSummary[] {
  return useFleet(
    useShallow((s) => {
      if (!nodeDid || !scopes.length) return NO_SESSIONS;
      const byScope = summariesByScope(s.sessionsOf[nodeDid]);
      return scopes.flatMap((key) => byScope.get(key) ?? []);
    }),
  );
}

/* The workers in a provenance view: the lineage owner's `started`. */
export function workersOf(
  provenance: Pick<SessionProvenanceView, "started">,
  sessions: readonly SessionSummary[] | undefined,
): Worker[] {
  const summaries = summariesByScope(sessions);
  return provenance.started.map((link) => ({
    sessionId: link.sessionId,
    nodeDid: link.nodeDid,
    summary: summaries.get(scopeKey(link)) ?? null,
    link,
  }));
}

const isBackgroundProcess = (t: RenderedToolCallView) =>
  t.presentation.kind === "process" && t.awaitMode === "background";

type Held<T> = { scope: string; value: T };

/** The selected session's provenance, read while this is shown (see
    `createProvenance`). */
export function useSessionProvenance(): SessionProvenanceView | null {
  const { stores, actions } = useApp();
  const { watchSessionProvenance } = actions;
  useEffect(() => watchSessionProvenance(), [watchSessionProvenance]);
  return stores.provenance.use.shown();
}

export function useWorkers(provenance: SessionProvenanceView | null): Workers {
  const {
    stores,
    actions: { fetchOperationsSnapshot },
  } = useApp();
  const facts = useSessionFacts();
  const nodeDid = stores.selection.use.nodeDid();
  const sessions = useListedScopes(
    nodeDid,
    provenance
      ? [
          ...provenance.started.map(scopeKey),
          ...provenance.calls.map((call) => scopeKey(call.caused)),
        ]
      : [],
  );
  const [heldOps, setHeldOps] = useState<Held<DesktopOperationsSnapshot> | null>(null);
  const ops = heldOps?.scope === nodeDid ? heldOps.value : null;
  /* asked again when a tool changes, which the session store counts */
  const tools = facts?.tools;
  const toolsRevision = facts?.toolsRevision ?? 0;
  const hasProcesses = useMemo(
    () => tools?.some(isBackgroundProcess) ?? false,
    [tools],
  );
  useEffect(() => {
    if (!hasProcesses || !nodeDid) {
      setHeldOps(null);
      return;
    }
    let live = true;
    void fetchOperationsSnapshot({ nodeDid }).then(
      (o) => live && setHeldOps({ scope: nodeDid, value: o }),
      () => live && setHeldOps(null),
    );
    return () => {
      live = false;
    };
  }, [fetchOperationsSnapshot, hasProcesses, nodeDid, toolsRevision]);
  return useMemo(() => {
    if (!provenance && !ops) return NO_WORKERS;
    const all = provenance ? workersOf(provenance, sessions) : [];
    const byScope = new Map(all.map((s) => [scopeKey(s.link), s]));
    const summaries = summariesByScope(sessions);
    const backgrounded = ops?.backgroundedTools ?? [];
    return {
      loaded: true,
      all,
      byToolCall: (tool) => {
        if (!tool.toolCallId || !tool.requestId) return null;
        const call = provenance?.calls.find(
          (c) => c.toolCallId === tool.toolCallId && c.requestId === tool.requestId,
        );
        if (!call) return null;
        return {
          request: call.caused,
          summary: summaries.get(scopeKey(call.caused)) ?? null,
          worker: byScope.get(scopeKey(call.caused)) ?? null,
        };
      },
      background: (tool) =>
        backgrounded.find(
          (b) => b.toolCallId === tool.toolCallId && b.requestId === tool.requestId,
        ) ?? null,
    };
  }, [provenance, ops, sessions]);
}
