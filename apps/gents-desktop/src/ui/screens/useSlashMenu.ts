import { useState, type KeyboardEvent } from "react";
import type { AgentContext, SkillView } from "@source-inc/gents-desktop-client";
import {
  applySkillSelection,
  effectiveContextSkills,
  matchingSkills,
  slashQueryAt,
} from "@source-inc/gents-desktop-chat";

export type SlashCommand = { id: "goal"; description: string; disabled?: boolean };

export type SlashItem =
  { kind: "command"; command: SlashCommand } | { kind: "skill"; skill: SkillView };

/** The menu a slash opens at the start of the composer: commands, which only
    begin a message, then the agent's skills. */
export function useSlashMenu(
  draft: string,
  setDraft: (next: string) => void,
  skills: SkillView[],
  context: AgentContext | null | undefined,
  available: readonly SlashCommand[],
  onCommand: (command: SlashCommand["id"]) => void,
) {
  const [index, setIndex] = useState(0);
  const [dismissedFor, setDismissedFor] = useState<string | null>(null);
  const at = dismissedFor === draft ? null : slashQueryAt(draft, draft.length);
  const commands =
    at && at.lineStart === 0
      ? available.filter((command) => command.id.startsWith(at.query))
      : [];
  const items: SlashItem[] = at
    ? [
        ...commands.map((command) => ({ kind: "command" as const, command })),
        ...matchingSkills(effectiveContextSkills(skills, context), at.query).map(
          (skill) => ({ kind: "skill" as const, skill }),
        ),
      ]
    : [];
  const active = items.length ? index % items.length : 0;
  const accept = (item: SlashItem) => {
    if (!at) return;
    if (item.kind === "command" && item.command.disabled) return;
    setIndex(0);
    if (item.kind === "command") {
      setDraft("");
      onCommand(item.command.id);
      return;
    }
    setDraft(
      applySkillSelection(draft, { ...at, items: [] }, item.skill.skillId).draft,
    );
  };
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!at) return;
    if (e.key === "Escape") {
      e.preventDefault();
      setDismissedFor(draft);
      return;
    }
    if (!items.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setIndex(
        (i) => (i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length,
      );
    } else if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
      e.preventDefault();
      accept(items[active]!);
    }
  };
  return {
    /** the skills a slash can name, which the field draws as chips */
    names: effectiveContextSkills(skills, context).map((skill) => skill.skillId),
    open: at !== null,
    query: at?.query ?? "",
    items,
    active,
    accept,
    onKeyDown,
  };
}
