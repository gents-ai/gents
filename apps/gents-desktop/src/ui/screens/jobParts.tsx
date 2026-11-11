/* The pieces a workflow run shows wherever it appears: the transcript cell
   and the run view. */

import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import { cn } from "@gents/ui/lib/utils";
import { focusDraft, writeDraft } from "../../hooks/draftStore";
import { sessionDraftKey } from "../../hooks/shellView";
import { navigate } from "@/lib/router";
import { useApp } from "../app/AppContext";

export const words = (value: string) => value.replace(/_/g, " ");

export const isTerminalJob = (state: JobState | null | undefined) =>
  state === "completed" ||
  state === "failed" ||
  state === "cancelled" ||
  state === "timed_out";

export function JobStatePill({ state }: { state: JobState }) {
  return (
    <span
      data-testid="job-state"
      className={cn(
        "rounded-full border border-border px-2 text-xs",
        state === "failed" && "text-destructive",
      )}
    >
      {words(state)}
    </span>
  );
}

/** The phases, the agents spent of the budget, the last event and, on a
    paused row, why it paused. */
export function JobProgressBody({
  state,
  progress,
}: {
  state: JobState | null | undefined;
  progress: JobProgress;
}) {
  return (
    <>
      <ol data-testid="job-phases" className="flex flex-wrap gap-x-3 text-xs">
        {progress.phases.map((phase, i) => (
          <li
            key={i}
            data-phase={phase.state}
            className={cn(
              phase.state === "pending" && "text-muted-foreground",
              phase.state === "active" && "font-medium",
              phase.state === "failed" && "text-destructive",
            )}
          >
            {phase.title}
          </li>
        ))}
      </ol>
      <div className="grid gap-1">
        <span data-testid="job-agents" className="text-xs text-muted-foreground">
          {progress.budget.spent} of {progress.budget.total} agents
        </span>
        <progress
          className="h-1 w-full"
          value={progress.budget.spent}
          max={progress.budget.total}
        />
      </div>
      {progress.last_event && (
        <p data-testid="job-last-event" className="text-xs text-muted-foreground">
          {progress.last_event}
        </p>
      )}
      {state === "paused" && progress.pause_reason && (
        <p data-testid="job-pause-reason" className="text-xs">
          {words(progress.pause_reason)}
        </p>
      )}
    </>
  );
}

/** Answer starts a composer draft naming the job; the person writes the rest.
    Given the run's session, a run answered from outside it opens that
    session with the draft. */
export function useAnswerJob() {
  const { drafts, stores, view } = useApp();
  return (
    handle: string | null | undefined,
    session?: { sessionId: string; nodeDid: string },
  ) => {
    const text = `Answer for ${handle}: `;
    const selected = stores.selection.getState();
    if (
      session &&
      (selected.sessionId !== session.sessionId || selected.nodeDid !== session.nodeDid)
    ) {
      const key = sessionDraftKey(session.nodeDid, session.sessionId);
      writeDraft(drafts, key, text);
      focusDraft(drafts, key);
      navigate({
        name: "session",
        sessionId: session.sessionId,
        nodeDid: session.nodeDid,
      });
      return;
    }
    writeDraft(drafts, view.getState().draftKey, text);
    document
      .querySelector<HTMLTextAreaElement>('[data-testid="composer"] textarea')
      ?.focus();
  };
}
