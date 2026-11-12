/* The pieces a workflow run shows wherever it appears: the transcript cell
   and the run view. */

import { useState, type ReactNode } from "react";
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
  const phases = progress.phases;
  const tone = (phase: JobPhase) =>
    cn(
      phase.state === "pending" && "text-muted-foreground/60",
      phase.state === "done" && "text-brand",
      phase.state === "active" && "text-foreground",
      phase.state === "failed" && "text-destructive",
    );
  const at = phases.findIndex((phase) => phase.state === "active");
  return (
    <div className="grid gap-1.5">
      {phases.length <= LABELLED_STAGES ? (
        <ol
          data-testid="job-phases"
          aria-label="Stages"
          className="flex flex-wrap items-start gap-x-2 gap-y-1.5"
        >
          {phases.map((phase, i) => (
            <li
              key={i}
              data-phase={phase.state}
              aria-current={phase.state === "active" ? "step" : undefined}
              className={cn("grid min-w-0 gap-0.5 text-xs", tone(phase))}
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
      ) : (
        /* many stages: one bar of short segments, named on hover, with only
           the current stage's name beneath */
        <div className="grid gap-0.5">
          <ol
            data-testid="job-phases"
            aria-label="Stages"
            className="flex max-w-full items-center gap-0.5 overflow-hidden"
          >
            {phases.map((phase, i) => (
              <li
                key={i}
                data-phase={phase.state}
                aria-current={phase.state === "active" ? "step" : undefined}
                title={phase.title}
                className={cn("flex text-xs", tone(phase))}
              >
                <AsciiProgress
                  aria-hidden
                  value={phase.state === "pending" ? 0 : 100}
                  cells={Math.max(1, Math.floor(COMPACT_CELLS / phases.length))}
                  className="text-[11px]"
                />
                <span className="sr-only">{phase.title}</span>
              </li>
            ))}
          </ol>
          {at >= 0 && (
            <p
              data-testid="job-stage"
              className="flex min-w-0 items-center gap-1 text-xs"
            >
              {moving && <AsciiLoader aria-hidden />}
              <span className="min-w-0 truncate font-medium">{phases[at]!.title}</span>
              <span className="shrink-0 text-muted-foreground">
                · stage {at + 1} of {phases.length}
              </span>
            </p>
          )}
        </div>
      )}
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
/* done stages the timeline keeps in view before the current one */
const SHOWN_DONE = 2;
/* past this many stages, their names no longer fit beside each other */
const LABELLED_STAGES = 4;
/* the many-stage bar's width in cells, shared by its segments */
const COMPACT_CELLS = 40;

/* the agents working now, by their sessions as this app lists them; one
   whose session isn't listed yet isn't drawn */
export function LiveAgents({ active }: { active: JobProgress["active"] }) {
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
  const phases = progress.phases;
  const last = phases.length - 1;
  const anyActive = phases.some((phase) => phase.state === "active");
  const [showDone, setShowDone] = useState(false);
  /* the done stages before the last few fold into one row, so the current
     stage stays near the top however many came before it */
  const reached = phases.findIndex((phase) => phase.state !== "done");
  const doneBefore = reached < 0 ? phases.length : reached;
  const folded = showDone ? 0 : Math.max(0, doneBefore - SHOWN_DONE);
  const hidden = folded >= 2 ? folded : 0;
  return (
    <div className="grid gap-2">
      <ol data-testid="job-phases" className="grid">
        {hidden > 0 && (
          <Stage
            node={<StageNode state="done" />}
            above={null}
            below="done"
            card="bg-accent/40"
          >
            <button
              type="button"
              data-testid="job-stages-folded"
              className="flex h-5 items-center justify-self-start text-sm text-brand underline decoration-brand/30 underline-offset-4 hover:decoration-brand"
              onClick={() => setShowDone(true)}
            >
              {hidden} stages done
            </button>
          </Stage>
        )}
        {phases.map((phase, i) => {
          if (i < hidden) return null;
          const active = phase.state === "active";
          const first = i === hidden && hidden === 0;
          return (
            <Stage
              key={i}
              data-stage={phase.state}
              node={<StageNode state={phase.state} />}
              above={first ? null : i === hidden ? "done" : phases[i - 1]!.state}
              below={i < last ? phase.state : null}
              card={cn(active ? "bg-accent/70 ring-1 ring-border" : "bg-accent/40")}
              current={active}
            >
              <span
                className={cn(
                  "flex h-5 items-center text-sm",
                  phase.state === "pending" && "text-muted-foreground",
                  phase.state === "done" && "text-brand",
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
                      <LiveAgents active={[child]} />
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
                <p
                  data-testid="job-last-event"
                  className="text-xs text-muted-foreground"
                >
                  {progress.last_event}
                </p>
              )}
            </Stage>
          );
        })}
      </ol>
      {!anyActive && progress.last_event && (
        <p data-testid="job-last-event" className="text-xs text-muted-foreground">
          {progress.last_event}
        </p>
      )}
    </div>
  );
}

/** How many of its budget the run has used; the agents working now show
    under their stage. */
export function JobBudget({ progress }: { progress: JobProgress }) {
  return (
    <p className="flex items-center gap-x-1.5 text-xs text-muted-foreground">
      <span data-testid="job-agents" className="tabular-nums">
        {progress.budget.spent} of {progress.budget.total} agents
      </span>
    </p>
  );
}

/* one stage on the timeline: its node on the line, the line's segments
   above and below it coloured by the stage they lead from, and its card */
function Stage({
  node,
  above,
  below,
  card,
  current = false,
  children,
  ...rest
}: {
  node: ReactNode;
  above: JobPhase["state"] | null;
  below: JobPhase["state"] | null;
  card: string;
  current?: boolean;
  children: ReactNode;
  "data-stage"?: string;
}) {
  const line = (state: JobPhase["state"]) =>
    state === "done" ? "bg-brand/50" : "bg-border";
  return (
    <li className="grid grid-cols-[1rem_1fr] gap-x-3" {...rest}>
      <span aria-hidden className="flex flex-col items-center">
        <span className={cn("h-2.5 w-px", above ? line(above) : "bg-transparent")} />
        <span className="grid h-5 place-items-center">{node}</span>
        {below && <span className={cn("w-px flex-1", line(below))} />}
      </span>
      <div className={cn("min-w-0", below && "pb-2")}>
        <div
          data-testid={current ? "job-stage-current" : undefined}
          className={cn("grid min-w-0 gap-1 rounded-lg px-3 py-2.5", card)}
        >
          {children}
        </div>
      </div>
    </li>
  );
}

function StageNode({ state }: { state: JobPhase["state"] }) {
  if (state === "done")
    return (
      <span className="grid size-4 place-items-center rounded-full bg-brand text-brand-foreground">
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
        <span aria-label="live" className="size-2 rounded-full bg-brand" />
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
