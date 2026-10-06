/* The sessions this one's calls reached. The runtime's lineage owner
   (`gents::session_origin::lineage`) decides which sessions this one started
   — "subagents" to the person — and which it only messaged; both are
   ordinary sessions to the runtime. The bridge maps that answer and the
   request each agents-tool call caused; this joins them to the session list
   for who each session is, and to this transcript's rows by the call. Every
   fact here has a field in the bridge contract; where the contract is silent
   the state says so instead of guessing. */
import { useEffect, useMemo, useRef, useState } from "react";
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
import { isLive } from "@/lib/live";
import {
  selectedIn,
  useSelectedSessionValue,
  useSessionFacts,
} from "../hooks/useSelectedSession";

export type Subagent = {
  sessionId: string;
  agentDid: string;
  summary: SessionSummary | null;
  /* the lineage owner's link: this session started it */
  link: LinkedSessionView;
};

/* what one agent_new/agent_message row reached */
export type Reached = {
  /* the request this row's call caused */
  request: CausedRequestView;
  summary: SessionSummary | null;
  /* the subagent, when the call reached a session this one started */
  subagent: Subagent | null;
};

export type Workers = {
  /* the sessions this one started */
  all: Subagent[];
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

/* A session is identified by its whole scope (agent, label, requester),
   never by its label alone. */
export const scopeKey = (r: {
  agentDid: string | null;
  sessionId: string | null;
  requesterDid: string | null;
}) => `${r.agentDid ?? ""}\u0000${r.sessionId ?? ""}\u0000${r.requesterDid ?? ""}`;

export const summariesByScope = (sessions: readonly SessionSummary[] | undefined) =>
  new Map((sessions ?? []).map((s) => [scopeKey(s), s]));

/* The sessions the node with this DID lists under these scopes, and only
   those: a change elsewhere in its list does not re-render the caller. */
export function useListedScopes(
  agentDid: string | null | undefined,
  scopes: readonly string[],
): readonly SessionSummary[] {
  return useFleet(
    useShallow((s) => {
      if (!agentDid || !scopes.length) return NO_SESSIONS;
      const byScope = summariesByScope(s.sessionsOf[agentDid]);
      return scopes.flatMap((key) => byScope.get(key) ?? []);
    }),
  );
}

/* The listed session with this agent and label. Two listed scopes under one
   label are ambiguous, and neither is picked. */
export function listedSession(
  sessions: readonly SessionSummary[] | undefined,
  agentDid: string | null,
  sessionId: string | null,
): SessionSummary | null {
  const matches = (sessions ?? []).filter(
    (s) => s.agentDid === agentDid && s.sessionId === sessionId,
  );
  return matches.length === 1 ? matches[0]! : null;
}

/* The subagents in a provenance view: the lineage owner's `started`. */
export function subagentsOf(
  provenance: Pick<SessionProvenanceView, "started">,
  sessions: readonly SessionSummary[] | undefined,
): Subagent[] {
  const summaries = summariesByScope(sessions);
  return provenance.started.map((link) => ({
    sessionId: link.sessionId,
    agentDid: link.agentDid,
    summary: summaries.get(scopeKey(link)) ?? null,
    link,
  }));
}

const isBackgroundProcess = (t: RenderedToolCallView) =>
  t.presentation.kind === "process" && t.awaitMode === "background";

type Held<T> = { scope: string; value: T };

/* The selected session's provenance. Its scope is exact: the session's
   agent, label and requester, as the session list reports them. It is asked
   again when the transcript's rows change (rowsRevision, which a streamed
   chunk to the live reply does not move), or the session list does. While a request it caused is live it is also asked
   whenever the client store's observation moves
   (projectionRevision.storeVersion), so a caused request settling anywhere
   this desktop observes refreshes it; there is no timer of its own. Every
   applied live delta advances storeVersion, so a settled lineage must not
   follow it: that would be one bridge call per streamed chunk. */
export function useSessionProvenance(): SessionProvenanceView | null {
  const { api, stores } = useApp();
  const sessionId = useSelectedSessionValue((s) => s?.sessionId ?? null);
  const rowsRevision = useSessionFacts()?.rowsRevision ?? 0;
  const agentDid = stores.selection.use.agentDid();
  const summary = useFleet((s) =>
    listedSession(agentDid ? s.sessionsOf[agentDid] : undefined, agentDid, sessionId),
  );
  const listed = summary !== null;
  const requesterDid = summary?.requesterDid ?? null;
  const scope = `${agentDid ?? ""}\u0000${sessionId ?? ""}\u0000${requesterDid ?? ""}`;
  const [held, setHeld] = useState<Held<SessionProvenanceView> | null>(null);
  const provenance = held?.scope === scope ? held.value : null;
  const awaitsCaused =
    provenance?.calls.some((c) => isLive(c.caused.lifecycleState)) ?? false;
  /* the store version moves with every streamed chunk, so it is followed,
     and re-renders this, only while a caused request is live */
  const storeVersion = useSelectedSessionValue((s) =>
    awaitsCaused ? (s?.projectionRevision?.storeVersion ?? null) : null,
  );
  const sessionsCue = useFleet((s) =>
    ((agentDid && s.sessionsOf[agentDid]) || NO_SESSIONS)
      .map((x) => `${x.sessionId}:${x.turnState ?? ""}:${x.updatedAt ?? ""}`)
      .join(),
  );
  /* what the last ask observed: a live lineage arriving starts following the
     store version without asking again for the one it was read at */
  const asked = useRef<{ cues: string; version: number | null } | null>(null);
  /* one ask out at a time: while the stream moves faster than a lineage read,
     a newer ask would outdate every answer before it lands. A cue that came
     while one was out is asked once that one lands. */
  const out = useRef(false);
  const missed = useRef(false);
  const [landed, setLanded] = useState(0);
  useEffect(() => {
    /* without the session's summary its exact scope is unknown */
    if (!agentDid || !sessionId || !listed) return;
    const cues = `${scope}\u0002${rowsRevision}\u0002${sessionsCue}`;
    const last = asked.current;
    if (
      last?.cues === cues &&
      (storeVersion === null || last.version === storeVersion)
    ) {
      return;
    }
    if (out.current) {
      missed.current = true;
      return;
    }
    asked.current = {
      cues,
      version:
        selectedIn(stores.session.getState(), {
          selectedSessionId: sessionId,
          selectedAgentDid: agentDid,
        })?.projectionRevision?.storeVersion ?? null,
    };
    out.current = true;
    void api
      .sessionProvenance({ sessionId, agentDid, requesterDid })
      .then(
        (value) => setHeld({ scope, value }),
        () => {
          /* the last known lineage stays; the next cue asks again */
          asked.current = null;
        },
      )
      .finally(() => {
        out.current = false;
        if (!missed.current) return;
        missed.current = false;
        setLanded((n) => n + 1);
      });
  }, [
    api,
    stores,
    agentDid,
    sessionId,
    requesterDid,
    listed,
    scope,
    storeVersion,
    rowsRevision,
    sessionsCue,
    landed,
  ]);
  return provenance;
}

export function useWorkers(provenance: SessionProvenanceView | null): Workers {
  const { api, stores } = useApp();
  const facts = useSessionFacts();
  const agentDid = stores.selection.use.agentDid();
  const sessions = useListedScopes(
    agentDid,
    provenance
      ? [
          ...provenance.started.map(scopeKey),
          ...provenance.calls.map((call) => scopeKey(call.caused)),
        ]
      : [],
  );
  const [heldOps, setHeldOps] = useState<Held<DesktopOperationsSnapshot> | null>(null);
  const ops = heldOps?.scope === agentDid ? heldOps.value : null;
  /* asked again when a tool changes, which the session store counts */
  const tools = facts?.tools;
  const toolsRevision = facts?.toolsRevision ?? 0;
  const hasProcesses = useMemo(
    () => tools?.some(isBackgroundProcess) ?? false,
    [tools],
  );
  useEffect(() => {
    if (!hasProcesses || !agentDid) {
      setHeldOps(null);
      return;
    }
    let live = true;
    void api.fetchOperationsSnapshot({ agentDid }).then(
      (o) => live && setHeldOps({ scope: agentDid, value: o }),
      () => live && setHeldOps(null),
    );
    return () => {
      live = false;
    };
  }, [api, hasProcesses, agentDid, toolsRevision]);
  return useMemo(() => {
    if (!provenance && !ops) return NO_WORKERS;
    const all = provenance ? subagentsOf(provenance, sessions) : [];
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
          subagent: byScope.get(scopeKey(call.caused)) ?? null,
        };
      },
      background: (tool) =>
        backgrounded.find(
          (b) => b.toolCallId === tool.toolCallId && b.requestId === tool.requestId,
        ) ?? null,
    };
  }, [provenance, ops, sessions]);
}
