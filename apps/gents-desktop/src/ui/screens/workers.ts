/* The sessions this one's tool calls started or messaged — "subagents" to the
   person, ordinary sessions to the runtime. Read from the durable request
   lineage (caused_by_parent_*), joined to the session list for who each one
   is and where it got to, and to this transcript's background tool rows for
   the call that reached it. Every fact here has a field in the bridge
   contract; where the contract is silent the state says so instead of
   guessing. */
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  BackgroundedToolView,
  CausedRequestView,
  DesktopOperationsSnapshot,
  RenderedToolCallView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { isLive } from "@/lib/live";

export type Subagent = {
  sessionId: string;
  agentDid: string | null;
  summary: SessionSummary | null;
  /* the requests this session's calls caused there, oldest first */
  requests: CausedRequestView[];
  /* the newest of them still running: the one a kill interrupts */
  live: CausedRequestView | null;
};

export type Workers = {
  /* every subagent of this session, in the order each was first reached */
  all: Subagent[];
  /* the subagent a create_session/send_message row reached, and the request
     that call caused there */
  byToolCall: (
    tool: RenderedToolCallView,
  ) => { subagent: Subagent; request: CausedRequestView } | null;
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

const byCreation = (a: CausedRequestView, b: CausedRequestView) =>
  (a.createdAt ?? "").localeCompare(b.createdAt ?? "") ||
  a.requestId.localeCompare(b.requestId);

/* The subagents in `sent`, grouped by the session each request landed in. */
export function subagentsOf(
  sent: readonly CausedRequestView[],
  sessions: readonly SessionSummary[] | undefined,
): Subagent[] {
  const summaries = new Map((sessions ?? []).map((s) => [s.sessionId, s]));
  const grouped = new Map<string, CausedRequestView[]>();
  for (const request of [...sent].sort(byCreation)) {
    if (!request.sessionId) continue;
    const seen = grouped.get(request.sessionId) ?? [];
    grouped.set(request.sessionId, [...seen, request]);
  }
  return [...grouped].map(([sessionId, requests]) => ({
    sessionId,
    agentDid: requests[requests.length - 1]!.agentDid,
    summary: summaries.get(sessionId) ?? null,
    requests,
    live: [...requests].reverse().find((r) => isLive(r.lifecycleState)) ?? null,
  }));
}

const isBackgroundProcess = (t: RenderedToolCallView) =>
  t.presentation.kind === "process" && t.awaitMode === "background";

/* While any subagent request is still running the lineage is asked again at
   most this often, besides on transcript and session-list changes: a session
   on another agent moves no local cue. */
export const LINEAGE_REFRESH_MS = 10_000;

type Held<T> = { scope: string; value: T };

/* The selected session's provenance: what it sent and what it received. It
   is asked for on transcript and session-list changes, and while any request
   it caused is still running, every LINEAGE_REFRESH_MS. */
export function useSessionProvenance(shell: Shell): SessionProvenanceView | null {
  const session = shell.selectedSession;
  const sessionId = session?.sessionId ?? null;
  const agentDid = shell.selectedDeployment?.agentDid ?? null;
  const sessions = shell.selectedDeployment?.sessions;
  const scope = `${agentDid ?? ""}\u0000${sessionId ?? ""}`;
  const [held, setHeld] = useState<Held<SessionProvenanceView> | null>(null);
  const provenance = held?.scope === scope ? held.value : null;
  /* a new turn, a tool state and a session-list move are the local cues;
     their values, not their identities, are what is compared */
  const cue = (session?.timelineItems ?? [])
    .map((i) =>
      i.kind === "toolGroup"
        ? i.tools.map((t) => `${t.itemKey}:${t.statusKind}`).join()
        : i.itemKey,
    )
    .join();
  const sessionsCue = (sessions ?? [])
    .map((s) => `${s.sessionId}:${s.turnState ?? ""}:${s.updatedAt ?? ""}`)
    .join();
  const unsettled = provenance?.sent.some((r) => isLive(r.lifecycleState)) ?? false;
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!unsettled) return;
    const timer = window.setInterval(() => setTick((t) => t + 1), LINEAGE_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [unsettled]);
  const generation = useRef(0);
  useEffect(() => {
    if (!agentDid || !sessionId) return;
    const ask = ++generation.current;
    void shell.api.sessionProvenance({ sessionId, agentDid }).then(
      (value) => {
        if (generation.current === ask) setHeld({ scope, value });
      },
      () => {
        /* the last known lineage stays; the next cue asks again */
      },
    );
  }, [shell.api, agentDid, sessionId, scope, cue, sessionsCue, tick]);
  return provenance;
}

export function useWorkers(
  shell: Shell,
  provenance: SessionProvenanceView | null,
): Workers {
  const session = shell.selectedSession;
  const agentDid = shell.selectedDeployment?.agentDid ?? null;
  const sessions = shell.selectedDeployment?.sessions;
  const [heldOps, setHeldOps] = useState<Held<DesktopOperationsSnapshot> | null>(null);
  const ops = heldOps?.scope === agentDid ? heldOps.value : null;
  const tools = useMemo(
    () =>
      session?.timelineItems.flatMap((i) => (i.kind === "toolGroup" ? i.tools : [])) ??
      [],
    [session?.timelineItems],
  );
  const hasProcesses = tools.some(isBackgroundProcess);
  const cue = tools.map((t) => `${t.itemKey}:${t.statusKind}`).join();
  useEffect(() => {
    if (!hasProcesses || !agentDid) {
      setHeldOps(null);
      return;
    }
    let live = true;
    void shell.api.fetchOperationsSnapshot({ agentDid }).then(
      (o) => live && setHeldOps({ scope: agentDid, value: o }),
      () => live && setHeldOps(null),
    );
    return () => {
      live = false;
    };
  }, [shell.api, hasProcesses, agentDid, cue]);
  return useMemo(() => {
    if (!provenance && !ops) return NO_WORKERS;
    const all = subagentsOf(provenance?.sent ?? [], sessions);
    const bySession = new Map(all.map((s) => [s.sessionId, s]));
    const backgrounded = ops?.backgroundedTools ?? [];
    return {
      loaded: true,
      all,
      byToolCall: (tool) => {
        if (!tool.toolCallId || !tool.requestId) return null;
        const request = provenance?.sent.find(
          (r) =>
            r.causedByToolCallId === tool.toolCallId &&
            r.causedByRequestId === tool.requestId,
        );
        const subagent = request?.sessionId ? bySession.get(request.sessionId) : null;
        return request && subagent ? { subagent, request } : null;
      },
      background: (tool) =>
        backgrounded.find(
          (b) => b.toolCallId === tool.toolCallId && b.requestId === tool.requestId,
        ) ?? null,
    };
  }, [provenance, ops, sessions]);
}
