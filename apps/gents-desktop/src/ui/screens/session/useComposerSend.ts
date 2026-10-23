import type { QueueDelivery } from "@source-inc/gents-desktop-client/generated/QueueDelivery";
import { useApp, useView } from "@/app/AppContext";
import { navigate } from "@/lib/router";
import { workspace } from "@/app/workspace";
import { useSelectedAgentId } from "@/hooks/useClient";
import { useDraft } from "../../../hooks/draftStore";

export function useAgentChoice() {
  const selectedAgentId = useSelectedAgentId();
  const { selectAgent } = useApp().actions;
  return {
    agentId: selectedAgentId,
    setPicked: selectAgent,
  };
}

export function useComposerSend(agentId: string | null | undefined) {
  const {
    drafts,
    stores,
    actions: { acceptsComposeIntent, captureComposeIntent, sendMessage },
  } = useApp();
  const draftKey = useView((view) => view.draftKey);
  const [draft, setDraft] = useDraft(drafts, draftKey);
  const selectedSessionId = stores.selection.use.sessionId();

  const send = async (text: string, followUp?: QueueDelivery) => {
    const pending = sendMessage(text, agentId, followUp);
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (result) setDraft((current) => (current === text ? "" : current));
    if (
      acceptsComposeIntent(intentGeneration) &&
      result &&
      result.sessionId !== selectedSessionId
    ) {
      if (!selectedSessionId) workspace.adoptNewSessionDock(result.sessionId);
      navigate({ name: "session", sessionId: result.sessionId });
    }
    return Boolean(result);
  };

  return { draft, setDraft, send };
}
