/* Which sessions sent work into this one: the session whose call started it,
   and the sender of each turn another session's call caused. Read from the
   durable request lineage; a turn's request names the call that caused it,
   so nothing is matched by text. Senders are ordinary sessions, and none of
   this confers hierarchy. */
import { useMemo } from "react";
import type {
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { behaviorName } from "./behavior";

export type Sender = {
  sessionId: string;
  /* the sending session as the runtime lists it; null when it is not listed
     on this deployment */
  summary: SessionSummary | null;
  behaviorName: string | null;
};

export type ParentWork = {
  /* the session whose call started this one */
  parent: Sender | null;
  /* the session whose call caused a turn's request; null for the person's own */
  sentBy: (requestId: string | null | undefined) => Sender | null;
  /* whether any turn here was sent by another session */
  hasSenders: boolean;
};

export const NO_PARENT: ParentWork = {
  parent: null,
  sentBy: () => null,
  hasSenders: false,
};

export function useParentWork(
  shell: Shell,
  provenance: SessionProvenanceView | null,
): ParentWork {
  const deployment = shell.selectedDeployment;
  return useMemo(() => {
    const received = [...(provenance?.received ?? [])].sort(
      (a, b) =>
        (a.createdAt ?? "").localeCompare(b.createdAt ?? "") ||
        a.requestId.localeCompare(b.requestId),
    );
    if (received.length === 0) return NO_PARENT;
    const sessions = new Map((deployment?.sessions ?? []).map((s) => [s.sessionId, s]));
    const senders = new Map<string, Sender>();
    const sender = (sessionId: string | null): Sender | null => {
      if (!sessionId) return null;
      let known = senders.get(sessionId);
      if (!known) {
        const summary = sessions.get(sessionId) ?? null;
        known = {
          sessionId,
          summary,
          behaviorName: summary ? behaviorName(summary.behaviorId, deployment) : null,
        };
        senders.set(sessionId, known);
      }
      return known;
    };
    const byRequest = new Map(
      received.map((r) => [r.requestId, r.causedBySessionId] as const),
    );
    /* the session's provenance names the request whose call started it; a
       session a person started and another session later messaged has none */
    const startedBy = sessions.get(provenance?.sessionId ?? "")?.provenance
      ?.parent_request_doc_id;
    const parent = startedBy
      ? sender(
          received.find((r) => r.causedByRequestDocId === startedBy)
            ?.causedBySessionId ?? null,
        )
      : null;
    return {
      parent,
      hasSenders: true,
      sentBy: (requestId) =>
        requestId ? sender(byRequest.get(requestId) ?? null) : null,
    };
  }, [provenance, deployment]);
}
