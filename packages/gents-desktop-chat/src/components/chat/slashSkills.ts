import type { AgentContext, SkillView } from "@source-inc/gents-desktop-client";


export type SlashSkillSuggestion = {
  query: string;
  lineStart: number;
  lineEnd: number;
  items: SkillView[];
};

export function effectiveContextSkills(
  skills: SkillView[],
  context: AgentContext | null | undefined,
): SkillView[] {
  if (!context) {
    return [];
  }

  const allowed = new Set(context.skill_ids ?? []);
  return skills.filter(
    (skill) => skill.enabled !== false && allowed.has(skill.skillId),
  );
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

/** The slash word being typed at the caret, on a line that only selectors
    precede, or null. */
export function slashQueryAt(
  draft: string,
  caret: number,
): { query: string; lineStart: number; lineEnd: number } | null {
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

  return { query: match[1].toLowerCase(), lineStart: start, lineEnd: end };
}

export function slashSkillSuggestion(
  draft: string,
  caret: number,
  skills: SkillView[],
): SlashSkillSuggestion | null {
  const at = slashQueryAt(draft, caret);
  if (!at) {
    return null;
  }

  const items = matchingSkills(skills, at.query);
  return items.length > 0 ? { ...at, items } : null;
}

export function matchingSkills(skills: SkillView[], query: string): SkillView[] {
  return skills
    .filter((skill) => skill.enabled !== false)
    .filter((skill) => {
      const haystack =
        `${skill.skillId} ${skill.displayName ?? ""} ${skill.name ?? ""}`.toLowerCase();
      return query === "" || haystack.includes(query);
    })
    .slice(0, 8);
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
