import { useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { workspace } from "@/app/workspace";
import { navigate } from "@/lib/router";
import {
  chatDraftRequest,
  chatDraftsFor,
  loadChatDrafts,
  type ChatDraft,
} from "../../../hooks/chatDrafts";

function description(draft: ChatDraft) {
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

export function ChatDeliveryFeedback() {
  const { stores } = useApp();
  const snapshot = stores.client.use.snapshot();
  const nodeDid = stores.selection.use.nodeDid();
  const sessionId = stores.selection.use.sessionId();
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
        snapshot?.client?.clientDid ?? null,
        nodeDid,
        sessionId,
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
  const notDelivered = draft.result?.outcome === "notDelivered";
  const confirmed = draft.result && !draft.result.outcome;
  async function recover(resume: boolean) {
    if (busy || disabled) return;
    const intent = actions.captureComposeIntent();
    setBusy(true);
    setError("");
    try {
      const result = await actions.recoverChatMessage(
        draft.requestId,
        resume,
        () => mounted.current,
      );
      if (!mounted.current) return;
      if (!result || result.outcome === "deliveryUnknown")
        setError(
          "Delivery is still unconfirmed. Check again or resume the saved message.",
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
      aria-label="Saved message"
      className="mt-2 rounded-xl border border-border p-3 text-xs"
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="mr-auto text-muted-foreground">
          {notDelivered
            ? "Message was not delivered"
            : confirmed
              ? "Message sent"
              : busy
                ? "Checking saved message…"
                : submitting
                  ? "Sending saved message…"
                  : "Message awaiting confirmation"}
        </span>
        <Button
          variant="ghost"
          size="sm"
          disabled={busy || disabled}
          onClick={() => void recover(false)}
        >
          {busy ? "Checking…" : "Check message delivery"}
        </Button>
        {notDelivered ? (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || disabled}
            onClick={() => void dismiss()}
          >
            Dismiss saved message
          </Button>
        ) : (
          !confirmed && (
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || disabled}
              onClick={() => void recover(true)}
            >
              Resume saved message
            </Button>
          )
        )}
      </div>
      <details className="mt-1 text-muted-foreground">
        <summary>Saved message</summary>
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
