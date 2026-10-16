import { useState, type KeyboardEvent } from "react";
import type { AgentContext, SkillView } from "@source-inc/gents-desktop-client";
import {
  applySkillSelection,
  effectiveContextSkills,
  slashSkillSuggestion,
} from "@source-inc/gents-desktop-chat";

export function useSlashSkills(
  draft: string,
  setDraft: (next: string) => void,
  skills: SkillView[],
  context: AgentContext | null | undefined,
) {
  const [index, setIndex] = useState(0);
  const [dismissedFor, setDismissedFor] = useState<string | null>(null);
  const available = effectiveContextSkills(skills, context);
  const suggestion =
    dismissedFor === draft
      ? null
      : slashSkillSuggestion(draft, draft.length, available);
  const items = suggestion?.items ?? [];
  const active = items.length ? index % items.length : 0;
  const accept = (skillId: string) => {
    if (!suggestion) return;
    setDraft(applySkillSelection(draft, suggestion, skillId).draft);
    setIndex(0);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!suggestion) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setIndex(
        (i) => (i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length,
      );
    } else if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      accept(items[active]!.skillId);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setDismissedFor(draft);
    }
  };
  return { items, active, accept, onKeyDown };
}
