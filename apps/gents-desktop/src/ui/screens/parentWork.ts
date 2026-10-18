import { useMemo } from "react";
import type {
  LinkedSessionView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { useSelectedNode } from "@/hooks/useClient";
import { agentName } from "./behavior";
import { scopeKey, summariesByScope, useListedScopes } from "./workers";

export type Sender = {
  sessionId: string;
  summary: SessionSummary | null;
  agentName: string | null;
};

export type ParentWork = {
  parent: Sender | null;
  sentBy: (requestId: string | null | undefined) => Sender | null;
  hasSenders: boolean;
};

export const NO_PARENT: ParentWork = {
  parent: null,
  sentBy: () => null,
  hasSenders: false,
};

export function useParentWork(provenance: SessionProvenanceView | null): ParentWork {
  const deployment = useSelectedNode();
  const sessions = useListedScopes(
    deployment?.nodeDid,
    provenance
      ? [
          ...(provenance.startedBy ? [scopeKey(provenance.startedBy)] : []),
          ...provenance.senders.map((turn) => scopeKey(turn.sender)),
        ]
      : [],
  );
  return useMemo(() => {
    if (!provenance?.startedBy && !provenance?.senders.length) return NO_PARENT;
    const summaries = summariesByScope(sessions);
    const sender = (link: LinkedSessionView): Sender => {
      const summary = summaries.get(scopeKey(link)) ?? null;
      return {
        sessionId: link.sessionId,
        summary,
        agentName: summary ? agentName(summary.agentId, deployment) : null,
      };
    };
    const byRequest = new Map(
      provenance.senders.map((turn) => [turn.requestId, sender(turn.sender)] as const),
    );
    return {
      parent: provenance.startedBy ? sender(provenance.startedBy) : null,
      hasSenders: byRequest.size > 0,
      sentBy: (requestId) => (requestId ? (byRequest.get(requestId) ?? null) : null),
    };
  }, [provenance, deployment, sessions]);
}
