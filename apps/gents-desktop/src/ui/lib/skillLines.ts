import type { ComposerPart } from "@gents/ui/conversation";

/* Gents takes a message's skills from its opening lines and strips those
   lines from what the agent reads (crates/gents/src/skills.rs,
   prompt_slash_skill_selection). A plain `/name` line is refused for names
   gents reserves for clients, such as review or plan, and a refused first
   line ends the list, so a skill is named as `/skill <id>`, which gents
   accepts for any id. Each skill chip in the message gets such a line: it
   runs wherever the chip sits, and the text reads as written. */

const SKILL_LINE = /^\/skill ([A-Za-z0-9_.-]+)$/;

/** the skills the message's chips name, in order, once each */
export function chipSkills(parts: readonly ComposerPart[], offered: readonly string[]) {
  const ids: string[] = [];
  for (const part of parts)
    if (
      part.type === "chip" &&
      part.chip.kind === "command" &&
      offered.includes(part.chip.id) &&
      !ids.includes(part.chip.id)
    )
      ids.push(part.chip.id);
  return ids;
}

/** `text` opened by `/<id>` for each of `skills`, one to a line with the
    last before the text, as a person types them, so the field draws each
    as a command chip */
export function withCommandLines(text: string, skills: readonly string[]): string {
  if (!skills.length) return text;
  const lines = skills.map((id) => `/${id}`);
  return `${lines.join("\n")} ${text}`;
}

const named = (text: string, id: string) =>
  new RegExp(`(^|\\s)/${id.replace(/[.-]/g, "\\$&")}(?=\\s|$)`).test(text);

/** a sent message's text as it was written, for copying: the skills it
    doesn't name open it as commands, which the composer draws as chips
    again when the text is pasted */
export function writtenText(
  content: string,
  skills: readonly string[] | null | undefined,
): string {
  const unnamed = [...new Set(skills ?? [])].filter((id) => !named(content, id));
  return withCommandLines(content, unnamed).trimEnd();
}

/** `text` with an opening `/skill <id>` line for each of `skills` */
export function withSkillLines(text: string, skills: readonly string[]): string {
  return skills.length
    ? `${skills.map((id) => `/skill ${id}`).join("\n")}\n${text}`
    : text;
}

/** what `withSkillLines` added, taken back off: the text and the skills, as
    gents will store them, for showing a message before gents has */
export function splitSkillLines(content: string): { text: string; skills: string[] } {
  const lines = content.split("\n");
  const skills: string[] = [];
  while (lines.length > 1) {
    const match = SKILL_LINE.exec(lines[0]!);
    if (!match) break;
    skills.push(match[1]!);
    lines.shift();
  }
  return { text: lines.join("\n"), skills };
}
