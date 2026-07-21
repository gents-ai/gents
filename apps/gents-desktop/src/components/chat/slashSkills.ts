import type { BehaviorView, SkillView } from "../../lib/types";

/// Composer support for the runtime's slash-skill convention: leading lines
/// of a prompt shaped like `/skill-id` are consumed as skill selection
/// (defra-agent skills::prompt_slash_skill_selection). This module only
/// decides when to SUGGEST — the runtime remains the parser of record.

export type SlashSkillSuggestion = {
  /** The partial token typed after "/" on the caret line. */
  query: string;
  /** Caret line's start/end offsets in the draft, for replacement. */
  lineStart: number;
  lineEnd: number;
  items: SkillView[];
};

/** Mirror the runtime's per-behavior skill candidate resolution. */
export function effectiveBehaviorSkills(
  skills: SkillView[],
  behavior: BehaviorView | null | undefined,
): SkillView[] {
  if (!behavior) {
    return [];
  }

  const refs = new Set(behavior.skillRefs);
  const excludes = new Set(behavior.skillExcludes);
  return skills.filter((skill) => {
    const scope = skill.scope?.trim();
    return (
      skill.enabled !== false &&
      !excludes.has(skill.skillId) &&
      (scope === "principal" || (scope === "behavior" && refs.has(skill.skillId)))
    );
  });
}

function lineBoundsAt(draft: string, caret: number): { start: number; end: number } {
  const start = draft.lastIndexOf("\n", caret - 1) + 1;
  const lineBreak = draft.indexOf("\n", caret);
  return { start, end: lineBreak === -1 ? draft.length : lineBreak };
}

const SELECTOR_LINE = /^\s*\/[\w.-]*\s*$/;

/**
 * Suggest skills when the caret sits on a leading selector line: every line
 * above must be blank or itself a selector (mirroring the runtime, which
 * only honors the leading block), and the caret line must be `/partial`.
 */
export function slashSkillSuggestion(
  draft: string,
  caret: number,
  skills: SkillView[],
): SlashSkillSuggestion | null {
  const { start, end } = lineBoundsAt(draft, caret);
  const line = draft.slice(start, end);
  const match = /^\s*\/([\w.-]*)$/.exec(line.slice(0, caret - start));
  if (!match) {
    return null;
  }

  for (const before of draft.slice(0, Math.max(start - 1, 0)).split("\n")) {
    if (before.trim() !== "" && !SELECTOR_LINE.test(before)) {
      return null;
    }
  }

  const query = match[1].toLowerCase();
  const items = skills
    .filter((skill) => skill.enabled !== false)
    .filter((skill) => {
      const haystack =
        `${skill.skillId} ${skill.displayName ?? ""} ${skill.name ?? ""}`.toLowerCase();
      return query === "" || haystack.includes(query);
    })
    .slice(0, 8);

  return items.length > 0 ? { query, lineStart: start, lineEnd: end, items } : null;
}

/** Replace the caret line with the chosen selector, keeping the body below. */
export function applySkillSelection(
  draft: string,
  suggestion: SlashSkillSuggestion,
  skillId: string,
): { draft: string; caret: number } {
  const selector = `/${skillId}`;
  const after = draft.slice(suggestion.lineEnd);
  const next =
    draft.slice(0, suggestion.lineStart) +
    selector +
    (after.startsWith("\n") ? "" : "\n") +
    after;
  return { draft: next, caret: suggestion.lineStart + selector.length + 1 };
}
