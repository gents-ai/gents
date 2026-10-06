/* The open session's header: where its work came from, its title, renamed
   in place, and its goal. */
import { useState } from "react";
import { ChevronDown, Pencil, Play, Target, Timer, Zap } from "lucide-react";
import type { GoalView } from "@source-inc/gents-desktop-client";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { Input } from "@gents/ui/components/input";
import { href } from "@/lib/router";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Hint } from "./Hint";

import { type ParentWork } from "./parentWork";
import { ArrowUpRight } from "lucide-react";
import { duration } from "./tool-summary";
import { toastFailure } from "@/lib/failure";

/* where this session came from: the session whose call started it, from
   provenance. It is an ordinary session; the link opens it as one. */
export function ParentLine({ work }: { work: ParentWork }) {
  if (!work.parent) return null;
  return (
    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
      Started by
      <a
        href={href({ name: "session", sessionId: work.parent.sessionId })}
        className="flex min-w-0 items-center gap-0.5 truncate hover:text-foreground hover:underline"
      >
        {work.parent.summary?.title ?? "another session"}
        <ArrowUpRight className="size-3 shrink-0" />
      </a>
    </span>
  );
}

/* A session nobody started by typing says who did. The list can filter on
   it and could not show it; the session itself said nothing at all, so a
   run that arrived overnight looked like one a person had asked for. */
export function StartedByAutomation({
  summary,
  agentDid,
}: {
  summary: SessionSummary;
  agentDid: string | null;
}) {
  const how =
    summary.triggerKind === "schedule"
      ? "on a schedule"
      : summary.triggerKind === "event"
        ? "by an event"
        : summary.triggerKind
          ? `by a ${summary.triggerKind}`
          : null;
  const name = summary.taskName ?? "a task";
  return (
    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
      {/* a task is a Play and a schedule is a Timer, the way the config
          names them: the mark is the thing it points at */}
      {summary.triggerKind === "schedule" ? (
        <Timer className="size-3 shrink-0" />
      ) : summary.triggerKind === "event" ? (
        <Zap className="size-3 shrink-0" />
      ) : (
        <Play className="size-3 shrink-0" />
      )}
      Started by
      {/* the task is a thing that exists and can be changed, so the name
         goes to it: reading why a run happened and deciding it should not
         happen again are the same errand */}
      {agentDid && summary.taskId ? (
        <a
          href={href({
            name: "agent",
            agentDid,
            section: "tasks",
            item: summary.taskId,
          })}
          className="truncate hover:text-foreground hover:underline"
        >
          {name}
        </a>
      ) : (
        <span className="truncate">{name}</span>
      )}
      {how && <span className="shrink-0">· {how}</span>}
    </span>
  );
}

/* the session's title, renamed in place the way the desktop's chat
   header does: a pencil beside it, Enter or blur saves, Escape reverts */
export function Title({
  title,
  onRename,
}: {
  title: string;
  onRename: (title: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(title);
  const submit = async () => {
    const next = draft.trim();
    setEditing(false);
    if (!next || next === title) {
      setDraft(title);
      return;
    }
    try {
      await onRename(next);
    } catch (e) {
      toastFailure("rename the session", e);
      setDraft(title);
    }
  };
  if (editing)
    return (
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Input
          autoFocus
          aria-label={`Rename ${title}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void submit()}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setDraft(title);
              setEditing(false);
            }
          }}
          className="h-8 w-[28rem] max-w-full font-heading text-lg font-medium text-heading"
        />
      </form>
    );
  return (
    <span className="group/title flex items-center gap-1">
      <h1 className="font-heading text-lg font-medium text-heading">{title}</h1>
      <Hint label="Rename">
        <Button
          variant="quiet"
          size="icon-xs"
          aria-label={`Rename ${title}`}
          className="opacity-0 transition-opacity group-hover/title:opacity-100 focus-visible:opacity-100"
          onClick={() => setEditing(true)}
        >
          <Pencil />
        </Button>
      </Hint>
    </span>
  );
}

/* The goal a session runs under, pinned above the composer rather than
   left at the top of the transcript to scroll away. A goal outlives every
   turn under it, so it belongs where the next turn is typed: what it is
   for, how long it has been at it, and — opened — what it has spent and
   what is holding it up.

   Every field here is the runtime's. What it cannot show is a plan: the
   projection carries an objective and a budget, not the steps toward it,
   so "how far along" is answered with elapsed time, continuations and
   spend rather than a checklist. */
export function Goal({ goal }: { goal: GoalView }) {
  const [open, setOpen] = useState(false);
  const used = goal.tokenBudget ? goal.tokensUsed / goal.tokenBudget : null;
  const blocked = goal.status === "blocked";
  /* Wrap-up also completes when a budget-limited goal stops, so only the
     runtime's status says whether the goal was met. */
  const state =
    goal.status === "complete"
      ? "met"
      : goal.status === "blocked"
        ? "blocked"
        : goal.status === "paused"
          ? "paused"
          : goal.status === "usage_limited"
            ? "· usage limit reached"
            : goal.status === "budget_limited"
              ? "· budget reached"
              : null;
  const wrapping = goal.wrapupRequested && !goal.wrapupCompleted;
  const phrase = [
    state ? `Goal ${state}` : "Goal",
    wrapping && goal.status !== "complete" ? "· wrapping up" : null,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="mb-2">
      <div
        className={cn(
          "flex items-center gap-2 rounded-xl border border-border bg-raised px-3 py-2 text-sm",
          blocked && "border-destructive/30",
        )}
      >
        <Target
          className={cn("size-4 shrink-0", blocked ? "text-destructive" : "text-ink")}
        />
        <span className={cn("shrink-0 font-medium", blocked && "text-destructive")}>
          {phrase}
        </span>
        {goal.objective && (
          <span
            className="min-w-0 truncate text-muted-foreground"
            title={goal.objective}
          >
            {goal.objective}
          </span>
        )}
        {goal.activeTimeSeconds > 0 && (
          <span className="ml-auto shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
            {duration(goal.activeTimeSeconds * 1000)}
          </span>
        )}
        <CollapsibleTrigger
          render={<Button variant="quiet" size="icon-xs" />}
          aria-label={open ? "Hide goal detail" : "Show goal detail"}
          className={cn("shrink-0", goal.activeTimeSeconds > 0 ? "" : "ml-auto")}
        >
          <ChevronDown
            className={cn("size-3.5 transition-transform", open && "rotate-180")}
          />
        </CollapsibleTrigger>
      </div>
      <CollapsibleContent className="px-3 pt-2">
        {/* what it has spent, which is the budget a goal runs against */}
        {used !== null && (
          <div className="grid gap-1">
            <div className="flex items-baseline justify-between text-xs text-muted-foreground">
              <span>Budget</span>
              <span className="font-mono tabular-nums">
                {Math.round(used * 100)}% of {Math.round(goal.tokenBudget! / 1000)}k
                tokens
              </span>
            </div>
            <div className="h-1 overflow-hidden rounded-full bg-muted">
              <div
                className={cn(
                  "h-full rounded-full",
                  used > 0.9 ? "bg-destructive" : "bg-brand",
                )}
                style={{ width: `${Math.min(100, Math.round(used * 100))}%` }}
              />
            </div>
          </div>
        )}
        <dl className="mt-2 grid gap-1 text-xs text-muted-foreground">
          {goal.continuationSequence > 0 && (
            <div className="flex justify-between gap-4">
              <dt>Continued</dt>
              <dd className="font-mono tabular-nums">
                {goal.continuationSequence}{" "}
                {goal.continuationSequence === 1 ? "time" : "times"}
              </dd>
            </div>
          )}
          {goal.status && (
            <div className="flex justify-between gap-4">
              <dt>Status</dt>
              <dd>{goal.status}</dd>
            </div>
          )}
        </dl>
        {goal.lastBlockedReason && (
          <p className="mt-2 text-xs text-destructive">
            Blocked: {goal.lastBlockedReason}
          </p>
        )}
        {goal.lastFailure && (
          <p className="mt-1 text-xs text-destructive">
            Last failure: {goal.lastFailure}
          </p>
        )}
        {goal.completionEvidence && (
          <p className="mt-2 text-xs text-muted-foreground">
            Evidence: {goal.completionEvidence}
          </p>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
}
