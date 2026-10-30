import type { RequestAttachment } from "@source-inc/gents-desktop-client";
import { useEffect, useRef, useState } from "react";
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
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const {
    drafts,
    stores,
    actions: { acceptsComposeIntent, captureComposeIntent, sendMessage, setGoal },
  } = useApp();
  const draftKey = useView((view) => view.draftKey);
  const [draft, setDraft] = useDraft(drafts, draftKey);
  const selectedSessionId = stores.selection.use.sessionId();
  const selectedNodeDid = stores.selection.use.nodeDid();
  const displayedIntent = captureComposeIntent();
  const [destinationError, setDestinationError] = useState<string | null>(null);
  const destinationMatches = () => {
    const current = stores.selection.getState();
    if (
      current.nodeDid === selectedNodeDid &&
      current.sessionId === selectedSessionId &&
      acceptsComposeIntent(displayedIntent)
    ) {
      setDestinationError(null);
      return true;
    }
    setDestinationError(
      "The message destination changed. Review it before sending again. Your draft is saved.",
    );
    return false;
  };
  const failure = stores.chat.use.submissionError();
  const error =
    destinationError ??
    (failure?.nodeDid === selectedNodeDid && failure?.sessionId === selectedSessionId
      ? failure.message
      : null);

  /* A first send adopts its session into the selection, which replaces the
     new-session composer before the send returns; the route must still move
     to the session, or New session stays the current route and does nothing. */
  const follow = (
    intentGeneration: ReturnType<typeof captureComposeIntent>,
    result: { sessionId: string } | null,
  ) => {
    const adopted =
      !selectedSessionId &&
      !!result &&
      stores.selection.getState().sessionId === result.sessionId;
    if (
      acceptsComposeIntent(intentGeneration) &&
      (mounted.current || adopted) &&
      result &&
      result.sessionId !== selectedSessionId
    ) {
      if (!selectedSessionId) workspace.adoptNewSessionDock(result.sessionId);
      navigate({
        name: "session",
        sessionId: result.sessionId,
        ...(selectedNodeDid ? { nodeDid: selectedNodeDid } : {}),
      });
    }
  };

  const goal = async (text: string, objective = text) => {
    if (!destinationMatches()) return false;
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const pending = setGoal(
      objective.replace(GOAL_COMMAND, "").trim(),
      agentId,
      () => mounted.current,
    );
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (
      !mounted.current ||
      stores.client.getState().snapshot?.client?.clientDid !== clientDid
    )
      return Boolean(result);
    if (result) setDraft((current) => (current === text ? "" : current));
    follow(intentGeneration, result);
    return Boolean(result);
  };

  /* `text` is the draft as typed, which decides a goal and is cleared once
     sent; `content` is what goes out when the composer adds to it, such as
     an opening line for a skill named mid-sentence */
  const send = async (
    text: string,
    supplied?: RequestAttachment[],
    followUp?: QueueDelivery,
    content = text,
  ) => {
    if (!destinationMatches()) return false;
    if (!supplied && GOAL_COMMAND.test(text)) return goal(text);
    const selected = drafts.getState().attachments[draftKey] ?? [];
    if (!supplied && selected.some((a) => !a.content || a.error)) return false;
    const attachments =
      supplied ?? selected.flatMap((a) => (a.content ? [a.content] : []));
    const ids = new Set(selected.map((a) => a.id));
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const pending = sendMessage(
      content,
      agentId,
      attachments,
      followUp,
      () => mounted.current,
    );
    const intentGeneration = captureComposeIntent();
    const result = await pending;
    if (stores.client.getState().snapshot?.client?.clientDid !== clientDid)
      return Boolean(result);
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
