import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { useEffect, useRef, useState } from "react";
import { useStore } from "zustand";
import { Button } from "@gents/ui/components/button";
import { errorWords } from "../../../lib/reasonText";
import { Notice } from "../../components/Notice";
import { useApp } from "@/app/AppContext";
import { workspace } from "@/app/workspace";
import { StorageFullRecovery } from "./StorageFullRecovery";
import { navigate } from "@/lib/router";
import {
  chatDraftRequest,
  chatDraftsFor,
  loadChatDrafts,
  type ChatDraft,
} from "../../../hooks/chatDrafts";

/* sending, and a confirmed delivery, need nothing from the person; a draft
   that needed them stays in view until it settles */
function needsAttention(draft: ChatDraft, inFlight: boolean) {
  const outcome = draft.result?.outcome;
  return (
    outcome === "notDelivered" ||
    outcome === "deliveryUnknown" ||
    (!draft.result && !inFlight)
  );
}

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
    return "The original input is unavailable. Checking delivery can still recover its outcome.";
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
  const requests = stores.chat.use.chatRequestsInFlight();
  const shown = useRef(new Set<string>());
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
      {chatDraftsFor(
        entries,
        clientDid,
        nodeDid,
        sessionId,
        mailboxItem?.itemId ?? null,
      )
        .filter((draft) => {
          if (needsAttention(draft, requests.has(draft.requestId)))
            shown.current.add(draft.requestId);
          return shown.current.has(draft.requestId);
        })
        .map((draft) => (
          <SavedMessage
            key={draft.requestId}
            draft={draft}
            disabled={sending || requests.has(draft.requestId)}
            submitting={requests.has(draft.requestId)}
          />
        ))}
    </>
  );
}

function SavedMessage({
  draft,
  disabled,
  submitting,
}: {
  draft: ChatDraft;
  disabled: boolean;
  submitting: boolean;
}) {
  const { actions, stores } = useApp();
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
  const notDelivered = draft.result?.outcome === "notDelivered";
  const confirmed = draft.result && !draft.result.outcome;
  async function recover(resume: boolean) {
    if (busy || disabled) return;
    const intent = actions.captureComposeIntent();
    setBusy(true);
    setError("");
    try {
      const result = draft.mailbox
        ? await actions.recoverMailboxAnswer(
            draft.mailbox.itemId,
            draft.requestId,
            resume,
            () => mounted.current,
          )
        : await actions.recoverChatMessage(
            draft.requestId,
            resume,
            () => mounted.current,
          );
      if (!mounted.current) return;
      if (!result || result.outcome === "deliveryUnknown")
        setError(
          `Delivery is still unconfirmed. Check again or resume the saved ${noun}.`,
        );
      else if (
        !result.outcome &&
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
  async function dismiss() {
    if (busy || disabled) return;
    setBusy(true);
    setError("");
    try {
      await actions.dismissChatMessage(draft.requestId);
    } catch (error) {
      if (mounted.current)
        setError(error instanceof Error ? error.message : String(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  return (
    <Notice
      tone={notDelivered ? "destructive" : "info"}
      role="status"
      aria-label={draft.mailbox ? "Saved answer" : "Saved message"}
      className="my-2"
      title={
        notDelivered
          ? draft.mailbox
            ? "The answer wasn't delivered"
            : "The message wasn't delivered"
          : confirmed
            ? draft.mailbox
              ? "Answer sent"
              : "Message sent"
            : busy
              ? `Checking the saved ${noun}…`
              : submitting
                ? `Sending the saved ${noun}…`
                : draft.mailbox
                  ? "Answer awaiting confirmation"
                  : "Message awaiting confirmation"
      }
      details={description(draft)}
      detailsText
      detailsLabel={`Saved ${noun}`}
      actions={
        <>
          <Button
            variant="outline"
            size="sm"
            disabled={busy || disabled}
            onClick={() => void recover(false)}
          >
            {busy ? "Checking…" : `Check ${noun} delivery`}
          </Button>
          {notDelivered ? (
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || disabled}
              onClick={() => void dismiss()}
            >
              Dismiss saved {noun}
            </Button>
          ) : (
            !confirmed && (
              <Button
                variant="ghost"
                size="sm"
                disabled={busy || disabled}
                onClick={() => void recover(true)}
              >
                Resume saved {noun}
              </Button>
            )
          )}
        </>
      }
    >
      {draft.result?.outcome === "deliveryUnknown" && (
        <StorageFullRecovery
          nodeDid={draft.nodeDid}
          error={draft.result?.deliveryError}
        />
      )}
      {error && (
        <p role="alert" tabIndex={0} className="text-destructive">
          {errorWords(error)}
        </p>
      )}
    </Notice>
  );
}
