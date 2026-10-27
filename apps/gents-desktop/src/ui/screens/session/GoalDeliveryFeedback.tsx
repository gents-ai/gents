import { useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { workspace } from "@/app/workspace";
import { navigate } from "@/lib/router";
import {
  goalDraftCommand,
  goalDraftsFor,
  loadGoalDrafts,
  type GoalDraft,
} from "../../../hooks/goalDrafts";

function description(draft: GoalDraft) {
  try {
    const action = goalDraftCommand(draft).action;
    if (action.kind === "create" || action.kind === "edit") return action.objective;
    return {
      pause: "Pause the Goal",
      resume: "Resume the Goal",
      stop: "Remove the Goal",
    }[action.kind];
  } catch {
    return "The original input is unavailable. Checking delivery can still recover its outcome.";
  }
}

export function GoalDeliveryFeedback() {
  const { stores } = useApp();
  const snapshot = stores.client.use.snapshot();
  const nodeDid = stores.selection.use.nodeDid();
  const sessionId = stores.selection.use.sessionId();
  const entries = stores.chat.use.goalDrafts();
  const readError = stores.chat.use.goalDraftError();
  const sending = stores.chat.use.sending();
  const commands = stores.chat.use.goalCommandsInFlight();
  const clientDid = snapshot?.client?.clientDid ?? null;
  useEffect(() => {
    const reload = () => stores.chat.setState(loadGoalDrafts());
    window.addEventListener("storage", reload);
    return () => window.removeEventListener("storage", reload);
  }, [stores.chat]);
  const drafts = goalDraftsFor(entries, clientDid, nodeDid, sessionId);
  return (
    <>
      {readError && (
        <p role="alert" className="mt-2 text-xs text-destructive">
          {readError}
        </p>
      )}
      {drafts.map((draft) => (
        <SavedGoal
          key={draft.commandId}
          draft={draft}
          disabled={sending || commands.has(draft.commandId)}
          submitting={commands.has(draft.commandId)}
        />
      ))}
    </>
  );
}

function SavedGoal({
  draft,
  disabled,
  submitting,
}: {
  draft: GoalDraft;
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
  async function recover(resume: boolean) {
    if (busy || disabled) return;
    const intent = actions.captureComposeIntent();
    setBusy(true);
    setError("");
    try {
      const result = await actions.recoverGoalChange(
        draft.commandId,
        resume,
        () => mounted.current,
      );
      if (!mounted.current) return;
      if (!result)
        setError(
          "This change is not confirmed yet. Check again or resume the saved change.",
        );
      else if (
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
      await actions.dismissGoalChange(draft.commandId);
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
      aria-label="Saved Goal change"
      className="mt-2 rounded-xl border border-border p-3 text-xs"
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="mr-auto text-muted-foreground">
          {draft.rejection
            ? "Goal change was not applied"
            : draft.result
              ? "Goal change saved"
              : busy
                ? "Checking saved Goal change…"
                : submitting
                  ? "Saving Goal change…"
                  : "Goal change awaiting confirmation"}
        </span>
        <Button
          variant="ghost"
          size="sm"
          disabled={busy || disabled}
          onClick={() => void recover(false)}
        >
          {busy ? "Checking…" : "Check Goal delivery"}
        </Button>
        {draft.rejection ? (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || disabled}
            onClick={() => void dismiss()}
          >
            Dismiss saved change
          </Button>
        ) : (
          <>
            {!draft.result && (
              <Button
                variant="ghost"
                size="sm"
                disabled={busy || disabled}
                onClick={() => void recover(true)}
              >
                Resume saved Goal change
              </Button>
            )}
          </>
        )}
      </div>
      <details className="mt-1 text-muted-foreground">
        <summary>Saved change</summary>
        <p className="mt-1 max-h-24 overflow-auto whitespace-pre-wrap break-words">
          {description(draft)}
        </p>
      </details>
      {(draft.rejection || error) && (
        <p
          role="alert"
          tabIndex={0}
          className="mt-2 max-h-24 overflow-auto whitespace-pre-wrap break-words text-destructive"
        >
          {draft.rejection?.reason ?? error}
        </p>
      )}
    </section>
  );
}
