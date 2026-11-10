import { useMemo } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import {
  parseMailboxQuestion,
  type MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { useApp } from "../app/AppContext";
import { chatDraftsFor } from "../../hooks/chatDrafts";
import { useFleet } from "./useFleet";

const NONE: readonly MailboxItemView[] = [];

/** The open questions the agent in a session asked this client, oldest
    first. The one place the session reads its questions from, so a change
    in who files them changes only this. */
export function useSessionQuestions(
  scope: { nodeDid: string; sessionId: string } | null,
): readonly MailboxItemView[] {
  return useFleet(
    useShallow((state) => {
      if (!scope) return NONE;
      const asked = Object.values(state.mailboxOf)
        .flat()
        .filter(
          (m) =>
            m.kind === "ask" &&
            m.status === "open" &&
            m.sessionId === scope.sessionId &&
            m.targetNodeDid === scope.nodeDid &&
            parseMailboxQuestion(m) !== null,
        );
      return asked.length
        ? asked.sort((a, b) => a.createdAt.localeCompare(b.createdAt))
        : NONE;
    }),
  );
}

/** The session's open questions this client hasn't answered yet: one sent
    from here waits for the runtime to close it, out of the way. */
export function useUnansweredQuestions(
  scope: { nodeDid: string; sessionId: string } | null,
): readonly MailboxItemView[] {
  const { stores } = useApp();
  const asked = useSessionQuestions(scope);
  const drafts = stores.chat.use.chatDrafts();
  const clientDid = useStore(
    stores.client,
    (state) => state.snapshot?.client?.clientDid ?? null,
  );
  return useMemo(() => {
    const open = asked.filter(
      (m) =>
        chatDraftsFor(drafts, clientDid, m.targetNodeDid, m.sessionId, m.itemId)
          .length === 0,
    );
    return open.length === asked.length ? asked : open;
  }, [asked, drafts, clientDid]);
}
