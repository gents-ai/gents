/* The pieces a workflow run shows wherever it appears: the transcript cell
   and the run view. */

import type { JobChildRow } from "@source-inc/gents-desktop-client/generated/JobChildRow";
import type { JobChildState } from "@source-inc/gents-desktop-client/generated/JobChildState";
import type { JobPhase } from "@source-inc/gents-desktop-client/generated/JobPhase";
import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";
import type { JobState } from "@source-inc/gents-desktop-client/generated/JobState";
import {
  Ban,
  Check,
  CircleCheck,
  CircleX,
  MessageCircleQuestion,
  Pause,
} from "lucide-react";
import { AsciiLoader, AsciiProgress } from "@gents/ui/components/ascii-loader";
import { cn } from "@gents/ui/lib/utils";
import { focusDraft, writeDraft } from "../../hooks/draftStore";
import { sessionDraftKey } from "../../hooks/shellView";
import { duration } from "@/lib/duration";
import { href, navigate } from "@/lib/router";
import { compactTokens } from "@/lib/tokens";
import { useApp } from "../app/AppContext";
import { useWorkerAgents, WorkerAgents } from "./WorkerAgents";

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
  /* a paused or waiting run holds its stage still */
  const moving = state === "running";
  return (
    <div className="grid gap-1.5">
      <ol
        data-testid="job-phases"
        aria-label="Stages"
        className="flex flex-wrap items-start gap-x-2 gap-y-1.5"
      >
        {progress.phases.map((phase, i) => (
          <li
            key={i}
            data-phase={phase.state}
            aria-current={phase.state === "active" ? "step" : undefined}
            className={cn(
              "grid min-w-0 gap-0.5 text-xs",
              phase.state === "pending" && "text-muted-foreground/60",
              phase.state === "done" && "text-brand",
              phase.state === "active" && "text-foreground",
              phase.state === "failed" && "text-destructive",
            )}
          >
            <AsciiProgress
              aria-hidden
              value={phase.state === "pending" ? 0 : 100}
              cells={STAGE_CELLS}
              className="text-[11px]"
            />
            <span
              className={cn(
                "inline-flex max-w-[12ch] items-center gap-1 truncate",
                phase.state === "active" && "font-medium",
              )}
            >
              {phase.state === "done" && <Check aria-hidden className="size-3" />}
              {phase.state === "active" && moving && <AsciiLoader aria-hidden />}
              {phase.title}
            </span>
          </li>
        ))}
      </ol>
      <p className="flex min-w-0 items-center gap-x-1.5 text-xs text-muted-foreground">
        <LiveAgents active={progress.active} />
        <span data-testid="job-agents" className="shrink-0 tabular-nums">
          {spent} of {total} agents
        </span>
        {progress.last_event && (
          <>
            <span aria-hidden>·</span>
            <span data-testid="job-last-event" className="min-w-0 truncate">
              {progress.last_event}
            </span>
          </>
        )}
      </p>
      {state === "paused" && progress.pause_reason && (
        <p data-testid="job-pause-reason" className="text-xs">
          Paused: {words(progress.pause_reason)}
        </p>
      )}
    </div>
  );
}

const STAGE_CELLS = 10;

/* the agents working now, by their sessions as this app lists them; one
   whose session isn't listed yet isn't drawn */
function LiveAgents({ active }: { active: JobProgress["active"] }) {
  const agents = useWorkerAgents((state) =>
    active.flatMap((child) => {
      const session = state.bySessionId[child.session_id];
      return session ? [session] : [];
    }),
  );
  if (agents.length === 0) return null;
  return (
    <span data-testid="job-live-agents" className="shrink-0">
      <WorkerAgents agents={agents} />
    </span>
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

/** A run's stages as a timeline: each stage a node on one line, the
    active one with the agents it is running and the last event. Which
    stage an agent belongs to isn't recorded, so only the active stage
    lists agents: the ones live now. */
export function JobStages({
  progress,
  onOpenSession,
}: {
  progress: JobProgress;
  onOpenSession?: () => void;
}) {
  const last = progress.phases.length - 1;
  return (
    <ol data-testid="job-stages" className="grid">
      {progress.phases.map((phase, i) => {
        const active = phase.state === "active";
        return (
          <li
            key={i}
            data-stage={phase.state}
            className="grid grid-cols-[1rem_1fr] gap-x-3"
          >
            <span className="flex flex-col items-center">
              <span className="grid h-5 place-items-center">
                <StageNode state={phase.state} />
              </span>
              {i < last && (
                <span
                  aria-hidden
                  className={cn(
                    "w-px flex-1",
                    phase.state === "done" ? "bg-foreground/40" : "bg-border",
                  )}
                />
              )}
            </span>
            <div className={cn("grid min-w-0 gap-1", i < last && "pb-4")}>
              <span
                className={cn(
                  "flex h-5 items-center text-sm",
                  phase.state === "pending" && "text-muted-foreground",
                  phase.state === "done" && "text-muted-foreground",
                  active && "font-medium",
                  phase.state === "failed" && "text-destructive",
                )}
              >
                {phase.title}
              </span>
              {active && progress.active.length > 0 && (
                <ul data-testid="job-stage-agents" className="grid gap-1">
                  {progress.active.map((child) => (
                    <li
                      key={child.slot}
                      className="flex min-w-0 items-center gap-2 text-xs"
                    >
                      <AsciiLoader className="text-foreground" />
                      <a
                        href={href({ name: "session", sessionId: child.session_id })}
                        onClick={onOpenSession}
                        className="min-w-0 truncate underline decoration-border underline-offset-4 hover:decoration-foreground"
                      >
                        {child.title || child.slot}
                      </a>
                    </li>
                  ))}
                </ul>
              )}
              {active && progress.last_event && (
                <p className="truncate text-xs text-muted-foreground">
                  {progress.last_event}
                </p>
              )}
            </div>
          </li>
        );
      })}
    </ol>
  );
}

function StageNode({ state }: { state: JobPhase["state"] }) {
  if (state === "done")
    return (
      <span className="grid size-4 place-items-center rounded-full bg-foreground text-background">
        <Check className="size-2.5" strokeWidth={3} />
      </span>
    );
  if (state === "active") return <AsciiLoader className="text-foreground" />;
  if (state === "failed") return <CircleX className="size-4 text-destructive" />;
  return <span className="size-2.5 rounded-full border border-muted-foreground/60" />;
}

/** The agents a run started, live ones first: state, tokens and time. */
export function JobAgents({
  agents,
  onOpenSession,
}: {
  agents: readonly JobChildRow[];
  onOpenSession?: () => void;
}) {
  const sorted = [...agents].sort(
    (a, b) => Number(b.state === "live") - Number(a.state === "live"),
  );
  return (
    <ul data-testid="run-children" className="grid divide-y divide-border/60">
      {sorted.map((child) => {
        const name = child.title || child.slot;
        return (
          <li
            key={child.slot}
            data-testid="run-child"
            data-state={child.state}
            className="flex items-center gap-3 py-2 text-sm"
          >
            <ChildMark state={child.state} />
            <span className="min-w-0 flex-1 truncate">
              {child.session_id ? (
                <a
                  href={href({ name: "session", sessionId: child.session_id })}
                  className="underline decoration-border underline-offset-4 hover:decoration-foreground"
                  onClick={onOpenSession}
                >
                  {name}
                </a>
              ) : (
                name
              )}
            </span>
            <span
              className={cn(
                "text-xs",
                child.state !== "live" && "text-muted-foreground",
              )}
            >
              {words(child.state)}
            </span>
            <span className="text-xs text-muted-foreground tabular-nums">
              {compactTokens(child.usage.total)} tokens
            </span>
            <span className="text-xs text-muted-foreground tabular-nums">
              {duration(child.duration_ms)}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function ChildMark({ state }: { state: JobChildState }) {
  return (
    <span className="grid size-4 shrink-0 place-items-center text-muted-foreground">
      {state === "live" ? (
        <AsciiLoader className="text-foreground" />
      ) : state === "completed" ? (
        <CircleCheck className="size-3.5" />
      ) : state === "failed" ? (
        <CircleX className="size-3.5 text-destructive" />
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
