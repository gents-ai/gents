/* The pieces a workflow run shows wherever it appears: the transcript cell
   and the run view. */

import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import {
  Ban,
  Check,
  ChevronRight,
  CircleCheck,
  CircleX,
  MessageCircleQuestion,
  Pause,
} from "lucide-react";
import { AsciiLoader, AsciiProgress } from "@gents/ui/components/ascii-loader";
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
  const { spent, total } = progress.budget;
  return (
    <div className="grid gap-1.5">
      <ol
        data-testid="job-phases"
        className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs"
      >
        {progress.phases.map((phase, i) => (
          <li key={i} data-phase={phase.state} className="flex items-center gap-1.5">
            {i > 0 && (
              <ChevronRight aria-hidden className="size-3 text-muted-foreground/50" />
            )}
            <span
              className={cn(
                "inline-flex items-center gap-1",
                (phase.state === "pending" || phase.state === "done") &&
                  "text-muted-foreground",
                phase.state === "active" && "font-medium text-foreground",
                phase.state === "failed" && "text-destructive",
              )}
            >
              {phase.state === "done" && <Check aria-hidden className="size-3" />}
              {phase.title}
            </span>
          </li>
        ))}
      </ol>
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <AsciiProgress
          aria-label="Agents used of the budget"
          value={total > 0 ? (Math.min(spent, total) / total) * 100 : 0}
          cells={16}
          className="text-[11px] text-foreground/80"
        />
        <span data-testid="job-agents" className="tabular-nums">
          {spent} of {total} agents
        </span>
      </div>
      {progress.last_event && (
        <p
          data-testid="job-last-event"
          className="truncate text-xs text-muted-foreground"
        >
          {progress.last_event}
        </p>
      )}
      {state === "paused" && progress.pause_reason && (
        <p data-testid="job-pause-reason" className="text-xs">
          Paused: {words(progress.pause_reason)}
        </p>
      )}
    </div>
  );
}

/** What a run's state looks like at the start of its row: the kit's loader
    while it runs, an icon otherwise. */
export function JobStateMark({ state }: { state: JobState | null | undefined }) {
  return (
    <span
      role="img"
      aria-label={state ? words(state) : "unknown"}
      data-testid="job-state-mark"
      className="grid size-4 shrink-0 place-items-center text-muted-foreground"
    >
      {state === "running" ? (
        <AsciiLoader className="text-foreground" />
      ) : state === "paused" ? (
        <Pause className="size-3.5" />
      ) : state === "awaiting_user" ? (
        <MessageCircleQuestion className="size-3.5 text-foreground" />
      ) : state === "completed" ? (
        <CircleCheck className="size-3.5" />
      ) : state === "failed" || state === "timed_out" ? (
        <CircleX className="size-3.5 text-destructive" />
      ) : state === "cancelled" ? (
        <Ban className="size-3.5" />
      ) : (
        <span className="size-1.5 rounded-full bg-border" />
      )}
    </span>
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
