import type { BehaviorView, SkillView } from "@source-inc/gents-desktop-client";


export type SlashSkillSuggestion = {
  query: string;
  lineStart: number;
  lineEnd: number;
  items: SkillView[];
};

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
      (scope === "principal" ||
        (scope === "behavior" && refs.has(skill.skillId)))
    );
  });
}

function lineBoundsAt(
  draft: string,
  caret: number,
): { start: number; end: number } {
  const start = draft.lastIndexOf("\n", caret - 1) + 1;
  const lineBreak = draft.indexOf("\n", caret);
  return { start, end: lineBreak === -1 ? draft.length : lineBreak };
}

const SELECTOR_LINE = /^\s*\/[\w.-]*\s*$/;

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

  return items.length > 0
    ? { query, lineStart: start, lineEnd: end, items }
    : null;
}

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
