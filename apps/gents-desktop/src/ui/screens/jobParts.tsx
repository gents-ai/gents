/* The pieces a workflow run shows wherever it appears: the transcript cell
   and the run view. */

import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import { cn } from "@gents/ui/lib/utils";
import { writeDraft } from "../../hooks/draftStore";
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

/** Answer starts a composer draft naming the job; the person writes the rest. */
export function useAnswerJob() {
  const { drafts, view } = useApp();
  return (handle: string | null | undefined) => {
    writeDraft(drafts, view.getState().draftKey, `Answer for ${handle}: `);
    document
      .querySelector<HTMLTextAreaElement>('[data-testid="composer"] textarea')
      ?.focus();
  };
}
