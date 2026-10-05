/* Which sessions sent work into this one: the session that started it, and
   the sender of each turn another session caused, both from the runtime's
   lineage owner as the bridge maps it. Nothing is matched by text. Senders
   are ordinary sessions, and none of this confers hierarchy. */
import { useMemo } from "react";
import type {
  LinkedSessionView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { useSelectedDeployment } from "@/hooks/useClient";
import { behaviorName } from "./behavior";
import { scopeKey, summariesByScope } from "./workers";

export type Sender = {
  sessionId: string;
  /* the sending session as the runtime lists it; null when it is not listed
     on this deployment */
  summary: SessionSummary | null;
  behaviorName: string | null;
};

export type ParentWork = {
  /* the session that started this one */
  parent: Sender | null;
  /* the session that sent a turn's request; null for the person's own */
  sentBy: (requestId: string | null | undefined) => Sender | null;
  /* whether any turn here was sent by another session */
  hasSenders: boolean;
};

export const NO_PARENT: ParentWork = {
  parent: null,
  sentBy: () => null,
  hasSenders: false,
};

export function useParentWork(provenance: SessionProvenanceView | null): ParentWork {
  const deployment = useSelectedDeployment();
  return useMemo(() => {
    if (!provenance?.startedBy && !provenance?.senders.length) return NO_PARENT;
    const summaries = summariesByScope(deployment?.sessions);
    const sender = (link: LinkedSessionView): Sender => {
      const summary = summaries.get(scopeKey(link)) ?? null;
      return {
        sessionId: link.sessionId,
        summary,
        behaviorName: summary ? behaviorName(summary.behaviorId, deployment) : null,
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
  }, [provenance, deployment]);
}
