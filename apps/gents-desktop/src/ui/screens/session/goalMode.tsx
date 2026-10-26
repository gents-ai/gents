import { useState, type KeyboardEvent } from "react";
import { Target, X } from "lucide-react";
import type { GoalView } from "@source-inc/gents-desktop-client";
import type { SlashCommand } from "../useSlashMenu";

/** A met or removed goal can be replaced; the runtime refuses to replace any
    other, and starts no goal while the session is busy. */
export const goalIsLive = (goal: GoalView | null | undefined) =>
  Boolean(goal) && goal?.status !== "complete" && goal?.status !== "stopped";

export const goalIsEditable = (goal: GoalView | null | undefined) =>
  goal?.status === "active" || goal?.status === "paused";

export function goalCommand(
  goal: GoalView | null | undefined,
  busy: boolean,
): SlashCommand {
  if (goalIsEditable(goal))
    return { id: "goal", description: "Edit this session's goal" };
  if (goalIsLive(goal))
    return {
      id: "goal",
      description: "Resume or remove this session's goal first",
      disabled: true,
    };
  if (busy)
    return {
      id: "goal",
      description: "Available when the agent finishes",
      disabled: true,
    };
  return {
    id: "goal",
    description: "Set a goal the agent works toward until it's met",
  };
}

type GoalMode = { kind: "create" } | { kind: "edit"; draft: string };

/** The composer holding a goal's objective instead of a message: `/goal`
    starts one, the goal banner's Edit changes the current one. */
export function useGoalMode({
  draft,
  setDraft,
  onSet,
  onEdit,
}: {
  draft: string;
  setDraft: (next: string) => void;
  onSet: (objective: string) => Promise<boolean>;
  onEdit?: (objective: string) => Promise<void>;
}) {
  const [mode, setMode] = useState<GoalMode | null>(null);
  const leave = () => {
    if (mode?.kind === "edit") setDraft(mode.draft);
    setMode(null);
  };
  const submit = async (text: string) => {
    if (mode?.kind === "edit" && onEdit) {
      await onEdit(text);
      setDraft(mode.draft);
      setMode(null);
      return;
    }
    if (await onSet(text)) setMode(null);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!mode) return;
    if (event.key === "Escape" || (event.key === "Backspace" && draft === "")) {
      event.preventDefault();
      leave();
    }
  };
  return {
    mode: mode?.kind ?? null,
    create: () => setMode({ kind: "create" }),
    edit: (objective: string) => {
      setMode({ kind: "edit", draft });
      setDraft(objective);
    },
    leave,
    submit,
    onKeyDown,
  };
}

export function GoalChip({
  editing,
  onCancel,
}: {
  editing: boolean;
  onCancel: () => void;
}) {
  return (
    <div className="flex items-center gap-2 px-2 pt-2" data-testid="goal-mode">
      <span className="inline-flex h-6 items-center gap-1.5 rounded-full border border-border bg-background pr-1 pl-2 text-xs text-muted-foreground">
        <Target className="size-3 shrink-0" />
        <span className="text-foreground">{editing ? "Editing goal" : "Goal"}</span>
        <button
          type="button"
          aria-label={editing ? "Cancel editing the goal" : "Leave goal mode"}
          title="Cancel (Esc)"
          onClick={onCancel}
          className="grid size-4 cursor-pointer place-items-center rounded-full hover:bg-accent hover:text-foreground"
        >
          <X className="size-3" />
        </button>
      </span>
    </div>
  );
}
