import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { useEffect, useRef, useState } from "react";
import { useStore } from "zustand";
import { Button } from "@gents/ui/components/button";
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
      ).map((draft) => (
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
    <section
      role="status"
      aria-label={draft.mailbox ? "Saved answer" : "Saved message"}
      className="mt-2 rounded-xl border border-border p-3 text-xs"
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="mr-auto text-muted-foreground">
          {notDelivered
            ? draft.mailbox
              ? "Answer was not delivered"
              : "Message was not delivered"
            : confirmed
              ? draft.mailbox
                ? "Answer sent"
                : "Message sent"
              : busy
                ? `Checking saved ${noun}…`
                : submitting
                  ? `Sending saved ${noun}…`
                  : draft.mailbox
                    ? "Answer awaiting confirmation"
                    : "Message awaiting confirmation"}
        </span>
        <Button
          variant="ghost"
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
      </div>
      {draft.result?.outcome === "deliveryUnknown" && (
        <StorageFullRecovery
          nodeDid={draft.nodeDid}
          error={draft.result?.deliveryError}
        />
      )}
      <details className="mt-1 text-muted-foreground">
        <summary>Saved {noun}</summary>
        <p className="mt-1 max-h-24 overflow-auto whitespace-pre-wrap break-words">
          {description(draft)}
        </p>
      </details>
      {error && (
        <p
          role="alert"
          tabIndex={0}
          className="mt-2 max-h-24 overflow-auto whitespace-pre-wrap break-words text-destructive"
        >
          {error}
        </p>
      )}
    </section>
  );
}
