import type {
  ChatSendRequest,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { useEffect, useRef, useState } from "react";
import { useStore } from "zustand";
import { Button } from "@gents/ui/components/button";
import { errorWords } from "../../../lib/reasonText";
import { Notice } from "../../components/Notice";
import { useApp } from "@/app/AppContext";
import {
  writeAttachments,
  writeDraft,
  type DraftStore,
} from "../../../hooks/draftStore";
import { newSessionDraftKey, sessionDraftKey } from "../../../hooks/shellView";
import { restoredAttachment } from "../../../hooks/useAttachments";
import { workspace } from "@/app/workspace";
import { StorageFullRecovery } from "./StorageFullRecovery";
import { navigate } from "@/lib/router";
import {
  chatDraftRequest,
  chatDraftsFor,
  loadChatDrafts,
  type ChatDraft,
} from "../../../hooks/chatDrafts";
import {
  deliveryAffordances,
  sendStatus,
  type SendStatus,
} from "../../../hooks/chatDeliveryView";
import { useSendDeadlineClock } from "../../../hooks/sendDeadlineClock";

function description(draft: ChatDraft) {
  if (draft.mailbox) return draft.mailbox.summary;
  try {
    const request = chatDraftRequest(draft);
    return [
      request.content,
      ...(request.attachments ?? []).map((attachment) => attachment.name),
    ]
      .filter(Boolean)
      .join("\n");
  } catch {
    return "The original input is unavailable.";
  }
}

export function ChatDeliveryFeedback({
  mailboxItem,
}: {
  mailboxItem?: MailboxItemView;
}) {
  const { stores } = useApp();
  const clientDid = useStore(
    stores.client,
    (state) => state.snapshot?.client?.clientDid ?? null,
  );
  const selectedNode = stores.selection.use.nodeDid();
  const selectedSession = stores.selection.use.sessionId();
  const nodeDid = mailboxItem?.targetNodeDid ?? selectedNode;
  const sessionId = mailboxItem ? mailboxItem.sessionId : selectedSession;
  const entries = stores.chat.use.chatDrafts();
  const readError = stores.chat.use.chatDraftError();
  const sending = stores.chat.use.sending();
  const now = useSendDeadlineClock(stores.chat);
  useEffect(() => {
    const reload = () => stores.chat.setState(loadChatDrafts());
    window.addEventListener("storage", reload);
    return () => window.removeEventListener("storage", reload);
  }, [stores.chat]);
  return (
    <>
      {readError && (
        <p role="alert" className="mt-2 text-xs text-destructive">
          {readError}
        </p>
      )}
      {/* every kept message is in view from the moment it is sent: pending
          while its send is in flight, failed with Retry and Discard, or a
          confirmed answer waiting for its question to close */}
      {chatDraftsFor(
        entries,
        clientDid,
        nodeDid,
        sessionId,
        mailboxItem?.itemId ?? null,
      ).map((draft) => {
        const status = sendStatus(draft, now);
        return (
          <SavedMessage
            key={draft.requestId}
            draft={draft}
            status={status}
            disabled={sending || status === "pending"}
          />
        );
      })}
    </>
  );
}

/** The composer gave a sent message its text and attachments, so Discard
 * hands them back to that composer (the message's own session, or the new
 * session composer for its agent), whichever screen is shown, unless
 * something new was composed there since. */
function handBack(drafts: DraftStore, draft: ChatDraft, request: ChatSendRequest) {
  const key = draft.selectedSessionId
    ? sessionDraftKey(draft.nodeDid, draft.selectedSessionId)
    : newSessionDraftKey(draft.nodeDid, request.agentDid ?? null);
  const state = drafts.getState();
  if (state.drafts[key] || state.attachments[key]?.length) return;
  if (request.content) writeDraft(drafts, key, request.content);
  const attachments = request.attachments ?? [];
  if (attachments.length)
    writeAttachments(drafts, key, () => attachments.map(restoredAttachment));
}

function SavedMessage({
  draft,
  status,
  disabled,
}: {
  draft: ChatDraft;
  status: SendStatus;
  disabled: boolean;
}) {
  const { actions, stores, drafts } = useApp();
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const noun = draft.mailbox ? "answer" : "message";
  const shown = deliveryAffordances(status, draft.mailbox !== undefined);
  async function retry() {
    if (busy || disabled) return;
    const intent = actions.captureComposeIntent();
    setBusy(true);
    setError("");
    try {
      const result = draft.mailbox
        ? await actions.retryMailboxAnswer(
            draft.mailbox.itemId,
            draft.requestId,
            () => mounted.current,
          )
        : await actions.retryChatMessage(draft.requestId, () => mounted.current);
      if (
        mounted.current &&
        actions.acceptsComposeIntent(intent) &&
        stores.selection.getState().sessionId === result.sessionId &&
        draft.selectedSessionId === null
      ) {
        workspace.adoptNewSessionDock(result.sessionId);
        navigate({ name: "session", sessionId: result.sessionId });
      }
    } catch (error) {
      if (mounted.current)
        setError(error instanceof Error ? error.message : String(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  async function discard() {
    if (busy || disabled) return;
    setBusy(true);
    setError("");
    try {
      if (draft.mailbox)
        await actions.discardMailboxAnswer(draft.mailbox.itemId, draft.requestId);
      else {
        const handed = await actions.discardChatMessage(draft.requestId);
        if (handed) handBack(drafts, draft, handed);
      }
    } catch (error) {
      if (mounted.current)
        setError(error instanceof Error ? error.message : String(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  const failed = shown.view === "failed";
  return (
    <Notice
      tone={failed ? "destructive" : "info"}
      role="status"
      aria-label={draft.mailbox ? "Saved answer" : "Saved message"}
      className="my-2"
      title={
        shown.view === "sending" || busy
          ? `Sending the ${noun}…`
          : shown.view === "delivered"
            ? draft.mailbox
              ? "Answer sent"
              : "Message sent"
            : draft.mailbox
              ? "The answer wasn't sent"
              : "The message wasn't sent"
      }
      details={description(draft)}
      detailsText
      detailsLabel={`Saved ${noun}`}
      actions={
        <>
          {shown.retry && (
            <Button
              variant="outline"
              size="sm"
              disabled={busy || disabled}
              onClick={() => void retry()}
            >
              {busy ? "Sending…" : `Retry ${noun}`}
            </Button>
          )}
          {shown.discard && (
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || disabled}
              onClick={() => void discard()}
            >
              Discard {noun}
            </Button>
          )}
        </>
      }
    >
      {failed && (
        <p>
          {draft.failure
            ? errorWords(draft.failure.message)
            : `No reply arrived for this ${noun}.`}{" "}
          Retry sends it again; it is never sent twice.
        </p>
      )}
      {failed && draft.failure?.error?.code === "storageFull" && (
        <StorageFullRecovery nodeDid={draft.nodeDid} error={draft.failure.error} />
      )}
      {error && (
        <p role="alert" tabIndex={0} className="text-destructive">
          {errorWords(error)}
        </p>
      )}
    </Notice>
  );
}
