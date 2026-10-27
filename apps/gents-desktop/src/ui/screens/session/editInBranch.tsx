import { useState } from "react";
import { Split } from "lucide-react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import type { Action } from "@gents/ui/conversation";
import { useApp, useView } from "@/app/AppContext";
import { useInterruptVisible } from "@/hooks/useClient";
import { toastFailure } from "@/lib/failure";
import { navigate } from "@/lib/router";
import { focusDraft, writeDraft } from "../../../hooks/draftStore";
import { sessionDraftKey } from "../../../hooks/shellView";

type SentUserMessage = Extract<RenderedTimelineItem, { kind: "userMessage" }>;

/* Core forks only at a user turn of a session this client may write, and
   refuses while the session has a request in flight. */
export function useEditInBranch(item: SentUserMessage | null): Action | null {
  const { actions, drafts, stores } = useApp();
  const sessionId = stores.selection.use.sessionId();
  const notOwned = useView((view) => {
    const status = view.shellProjection.nonEmptyContentSendStatus;
    return status.kind === "disabled" && status.reason === "sessionNotOwned";
  });
  const working = useInterruptVisible();
  const [forking, setForking] = useState(false);
  const text = item?.content;
  if (!item || !sessionId || !text || notOwned) return null;
  return {
    label: "Fork",
    icon: <Split />,
    disabled: forking || working,
    onClick: async () => {
      setForking(true);
      const intent = actions.captureComposeIntent();
      try {
        const branch = await actions.forkBeforeMessage(sessionId, item.itemKey);
        const key = sessionDraftKey(branch.nodeDid, branch.sessionId);
        writeDraft(drafts, key, text);
        if (!actions.acceptsComposeIntent(intent)) return;
        focusDraft(drafts, key);
        navigate({ name: "session", sessionId: branch.sessionId });
      } catch (error) {
        toastFailure("start a branch", error);
      } finally {
        setForking(false);
      }
    },
  };
}
