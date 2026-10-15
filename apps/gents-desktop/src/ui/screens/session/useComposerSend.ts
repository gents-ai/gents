/* The composer's draft and its send, shared by both session views. The
   draft is read here, in the composer's own component, so a keystroke
   re-renders the composer and not the screen around it. */
import { useApp, useView } from "@/app/AppContext";
import { navigate } from "@/lib/router";
import { workspace } from "@/app/workspace";
import { useSelectedAgentId } from "@/hooks/useClient";
import { useDraft } from "../../../hooks/draftStore";

export function useAgentChoice() {
  const selectedAgentId = useSelectedAgentId();
  const { selectAgent } = useApp().actions;
  return {
    // Read the same effective selection that owns composer admission. Defaults,
    // mailbox routing, and agent changes are resolved by the selection, not here.
    agentId: selectedAgentId,
    setPicked: selectAgent,
  };
}

/** The draft for the screen's draft key, and a send that empties it. */
export function useComposerSend(agentId: string | null | undefined) {
  const {
    drafts,
    stores,
    actions: { acceptsComposeIntent, captureComposeIntent, sendMessage },
  } = useApp();
  const draftKey = useView((view) => view.draftKey);
  const [draft, setDraft] = useDraft(drafts, draftKey);
  const selectedSessionId = stores.selection.use.sessionId();

  /** sends the text; true once it is accepted */
  const send = async (text: string) => {
    const pending = sendMessage(text, agentId);
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
