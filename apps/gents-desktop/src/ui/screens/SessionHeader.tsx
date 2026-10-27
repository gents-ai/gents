import { useState } from "react";
import { ChevronDown, Pause, Pencil, Play, Target, Timer, X, Zap } from "lucide-react";
import type { GoalControlAction, GoalView } from "@source-inc/gents-desktop-client";
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

export function StartedByAutomation({
  summary,
  nodeDid,
}: {
  summary: SessionSummary;
  nodeDid: string | null;
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
      {summary.triggerKind === "schedule" ? (
        <Timer className="size-3 shrink-0" />
      ) : summary.triggerKind === "event" ? (
        <Zap className="size-3 shrink-0" />
      ) : (
        <Play className="size-3 shrink-0" />
      )}
      Started by
      {nodeDid && summary.taskId ? (
        <a
          href={href({
            name: "agent",
            nodeDid,
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

/** a title edited in place: Enter or leaving it renames, Escape keeps it */
export function RenameField({
  title,
  onRename,
  onDone,
  className,
}: {
  title: string;
  onRename: (title: string) => Promise<void>;
  onDone: () => void;
  className?: string;
}) {
  const [draft, setDraft] = useState(title);
  const submit = async () => {
    const next = draft.trim();
    onDone();
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
            onDone();
          }
        }}
        className={className}
      />
    </form>
  );
}

export function Title({
  title,
  onRename,
  editing: held,
  onEditingChange,
}: {
  title: string;
  onRename: (title: string) => Promise<void>;
  /** held by the screen where its menu can start the rename too */
  editing?: boolean;
  onEditingChange?: (editing: boolean) => void;
}) {
  const [own, setOwn] = useState(false);
  const editing = held ?? own;
  const setEditing = onEditingChange ?? setOwn;
  if (editing)
    return (
      <RenameField
        title={title}
        onRename={onRename}
        onDone={() => setEditing(false)}
        className="h-8 w-[28rem] max-w-full font-heading text-lg font-medium text-heading"
      />
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

/** The controls the runtime's goal transitions allow from each status; it
    refuses any other with a reason. */
const GOAL_CONTROLS: Record<string, readonly ("pause" | "resume" | "edit" | "stop")[]> =
  {
    active: ["pause", "edit", "stop"],
    budget_limited: ["pause", "stop"],
    paused: ["resume", "edit", "stop"],
    blocked: ["resume", "stop"],
    usage_limited: ["resume", "stop"],
  };

export function Goal({
  goal,
  onEdit,
  onControl,
}: {
  goal: GoalView;
  onEdit?: () => void;
  onControl?: (action: Exclude<GoalControlAction, { kind: "create" | "edit" }>) => void;
}) {
  const [open, setOpen] = useState(false);
  const controls = onControl ? (GOAL_CONTROLS[goal.status ?? ""] ?? []) : [];
  const used = goal.tokenBudget ? goal.tokensUsed / goal.tokenBudget : null;
  const blocked = goal.status === "blocked";
  /* A budget-limited stop also completes wrap-up; runtime status records
     whether the goal was met. */
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
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className={cn(
        "mb-2 rounded-xl border border-border bg-raised text-sm",
        blocked && "border-destructive/30",
      )}
    >
      <div className="flex items-center gap-2 px-3 py-2">
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
        {controls.length > 0 && (
          <span
            className={cn(
              "flex shrink-0 items-center",
              goal.activeTimeSeconds > 0 ? "" : "ml-auto",
            )}
          >
            {controls.includes("pause") && (
              <Hint label="Pause goal">
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Pause goal"
                  onClick={() => onControl?.({ kind: "pause" })}
                >
                  <Pause />
                </Button>
              </Hint>
            )}
            {controls.includes("resume") && (
              <Hint label="Resume goal">
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Resume goal"
                  onClick={() => onControl?.({ kind: "resume" })}
                >
                  <Play />
                </Button>
              </Hint>
            )}
            {controls.includes("edit") && onEdit && (
              <Hint label="Edit goal">
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Edit goal"
                  onClick={onEdit}
                >
                  <Pencil />
                </Button>
              </Hint>
            )}
            {controls.includes("stop") && (
              <Hint label="Remove goal">
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Remove goal"
                  onClick={() => onControl?.({ kind: "stop" })}
                >
                  <X />
                </Button>
              </Hint>
            )}
          </span>
        )}
        <CollapsibleTrigger
          render={<Button variant="quiet" size="icon-xs" />}
          aria-label={open ? "Hide goal detail" : "Show goal detail"}
          className={cn(
            "shrink-0",
            goal.activeTimeSeconds > 0 || controls.length > 0 ? "" : "ml-auto",
          )}
        >
          <ChevronDown
            className={cn("size-3.5 transition-transform", open && "rotate-180")}
          />
        </CollapsibleTrigger>
      </div>
      <CollapsibleContent className="max-h-48 overflow-y-auto border-t border-border/60 px-3 py-2">
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
