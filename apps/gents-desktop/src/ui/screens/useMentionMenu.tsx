import { useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import { MessageSquare } from "lucide-react";
import {
  ChipCard,
  formatReference,
  referenceKind,
  type ChipKind,
  type ComposerTrigger,
  type ComposerTriggerItem,
} from "@gents/ui/conversation";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { matchScore, queryWords } from "../lib/search";
import { useFleet } from "../hooks/useFleet";
import { agentName } from "./behavior";
import { when } from "./time";

type MentionRow = ComposerTriggerItem & { session: SessionSummary };

const SHOWN = 8;

const titleOf = (session: SessionSummary) => session.title?.trim() || "Untitled";

const newestFirst = (a: SessionSummary, b: SessionSummary) =>
  (b.updatedAt ?? "").localeCompare(a.updatedAt ?? "");

/** How a session reference looks, in the composer and on a sent message:
    its card names the session's agent and shows its latest text. */
export function useSessionChipKind(nodeDid: string | null): ChipKind {
  const { sessions, node } = useFleet(
    useShallow((s) => ({
      sessions: s.sessions,
      node: nodeDid ? (s.nodes[nodeDid] ?? null) : null,
    })),
  );
  return useMemo(
    () =>
      referenceKind("session", {
        icon: <MessageSquare />,
        preview: (chip) => {
          const session = Object.values(sessions).find(
            (s) => s.nodeDid === nodeDid && s.sessionId === chip.id,
          );
          return (
            <ChipCard
              title={chip.label}
              subtitle={
                session ? `Session · ${agentName(session.agentId, node)}` : "Session"
              }
            >
              {session?.previewText ?? undefined}
            </ChipCard>
          );
        },
      }),
    [sessions, node, nodeDid],
  );
}

/** The menu @ opens in the composer: the node's other sessions, the most
    recent first, or those whose title matches what follows the @. A session
    picked becomes a chip that sends `[title](gents:session/<id>)`, which the
    agent opens with its sessions tool on the same node. */
export function useMentionMenu(nodeDid: string | null, sessionId: string | null) {
  const { sessions, node } = useFleet(
    useShallow((s) => ({
      sessions: s.sessions,
      node: nodeDid ? (s.nodes[nodeDid] ?? null) : null,
    })),
  );
  const others = useMemo(
    () =>
      Object.values(sessions)
        .filter(
          (s) => s.nodeDid === nodeDid && s.sessionId !== sessionId && !s.archivedAt,
        )
        .sort(newestFirst)
        .map((session) => ({
          session,
          hay: titleOf(session).toLowerCase(),
          agent: agentName(session.agentId, node),
        })),
    [sessions, node, nodeDid, sessionId],
  );
  const trigger: ComposerTrigger<MentionRow> = {
    char: "@",
    at: "word",
    label: "Sessions",
    empty: (query) => `No sessions match @${query}`,
    search: (query) => {
      const words = queryWords(query);
      const now = Date.now();
      return others
        .map((other) => ({
          ...other,
          score: words.length ? matchScore(words, other.hay) : 1,
        }))
        .filter((other) => other.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, SHOWN)
        .map(({ session, agent }) => ({
          key: `session:${session.sessionId}`,
          label: titleOf(session),
          detail: [agent, when(session.updatedAt, now)].filter(Boolean).join(" · "),
          icon: <MessageSquare />,
          group: "Sessions",
          session,
        }));
    },
    select: ({ session }) => {
      const reference = {
        kind: "session",
        id: session.sessionId,
        label: titleOf(session),
      };
      return { ...reference, text: formatReference(reference) };
    },
  };
  const session = useSessionChipKind(nodeDid);
  const chipKinds = useMemo<Record<string, ChipKind>>(() => ({ session }), [session]);
  return { trigger, chipKinds };
}
