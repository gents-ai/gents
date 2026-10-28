/* Gents takes a message's skills from its opening lines, each starting with
   `/name` (crates/gents/src/skills.rs, prompt_slash_skill_selection), and
   strips those lines from what the agent reads. A skill named later, such as
   a chip in mid-sentence, would otherwise arrive as plain text, so it gets an
   opening line of its own: it runs, and the text still reads as written. */

const OPENING = /^\s*\/([A-Za-z0-9_.-]+)(?![A-Za-z0-9_./-])/;

/** the skills the opening lines already name, as gents reads them */
function openingSkills(text: string): string[] {
  const named: string[] = [];
  for (const line of text.split("\n")) {
    if (!named.length && line.trim() === "") continue;
    const match = OPENING.exec(line);
    if (!match) break;
    named.push(match[1]!);
  }
  return named;
}

/** `text` with an opening line for each of `skills` it names further on */
export function withSkillLines(text: string, skills: readonly string[]): string {
  const known = new Set(skills);
  const opening = new Set(openingSkills(text));
  const later: string[] = [];
  for (const match of text.matchAll(/(^|\s)\/([^\s/]+)(?=\s|$)/g)) {
    const name = match[2]!;
    if (known.has(name) && !opening.has(name) && !later.includes(name))
      later.push(name);
  }
  return later.length ? `${later.map((name) => `/${name}`).join("\n")}\n${text}` : text;
}
