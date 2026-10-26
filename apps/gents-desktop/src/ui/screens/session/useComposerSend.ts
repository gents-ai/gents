import type { RequestAttachment } from "@source-inc/gents-desktop-client";
import { writeAttachments } from "../../../hooks/draftStore";
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

/** `/goal` and a space begin a goal's objective, typed or in goal mode. */
const GOAL_COMMAND = /^\s*\/goal\s+(?=\S)/;

export function useComposerSend(agentId: string | null | undefined) {
  const {
    drafts,
    stores,
    actions: { acceptsComposeIntent, captureComposeIntent, sendMessage, setGoal },
  } = useApp();
  const draftKey = useView((view) => view.draftKey);
  const [draft, setDraft] = useDraft(drafts, draftKey);
  const selectedSessionId = stores.selection.use.sessionId();
  const selectedNodeDid = stores.selection.use.nodeDid();
  const failure = stores.chat.use.submissionError();
  const error =
    failure?.nodeDid === selectedNodeDid && failure?.sessionId === selectedSessionId
      ? failure.message
      : null;

  const follow = (
    intentGeneration: ReturnType<typeof captureComposeIntent>,
    result: { sessionId: string } | null,
  ) => {
    if (
      acceptsComposeIntent(intentGeneration) &&
      result &&
      result.sessionId !== selectedSessionId
    ) {
      if (!selectedSessionId) workspace.adoptNewSessionDock(result.sessionId);
      navigate({ name: "session", sessionId: result.sessionId });
    }
  };

  const goal = async (text: string, objective = text) => {
    const pending = setGoal(objective.replace(GOAL_COMMAND, "").trim(), agentId);
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (result) setDraft((current) => (current === text ? "" : current));
    follow(intentGeneration, result);
    return Boolean(result);
  };

  const send = async (
    text: string,
    supplied?: RequestAttachment[],
    followUp?: QueueDelivery,
  ) => {
    if (!supplied && GOAL_COMMAND.test(text)) return goal(text);
    const selected = drafts.getState().attachments[draftKey] ?? [];
    if (!supplied && selected.some((a) => !a.content || a.error)) return false;
    const attachments =
      supplied ?? selected.flatMap((a) => (a.content ? [a.content] : []));
    const ids = new Set(selected.map((a) => a.id));
    const pending = sendMessage(text, agentId, attachments, followUp);
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (result) {
      if (!supplied) setDraft((current) => (current === text ? "" : current));
      if (!supplied)
        writeAttachments(drafts, draftKey, (current) =>
          current.filter((a) => !ids.has(a.id)),
        );
    }
    follow(intentGeneration, result);
    return Boolean(result);
  };

  return { draft, setDraft, send, setGoal: goal, error };
}
