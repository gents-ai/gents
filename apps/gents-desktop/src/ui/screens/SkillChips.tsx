import { useMemo } from "react";
import {
  Chip,
  ChipCard,
  parseParts,
  type ChipKind,
  type ComposerChip,
} from "@gents/ui/conversation";
import type { SkillView } from "@source-inc/gents-desktop-client";
import { useSelectedNode } from "@/hooks/useClient";

const folderName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/** The card over a skill or command chip: what it is and what it does. */
export function skillCard(
  chip: ComposerChip,
  skills: readonly SkillView[],
  commands: readonly { id: string; description: string }[] = [],
) {
  const skill = skills.find((s) => s.skillId === chip.id);
  if (skill)
    return (
      <ChipCard
        title={skill.displayName ?? skill.name ?? chip.label}
        subtitle={
          skill.sourceDirectory
            ? `Skill · from ${folderName(skill.sourceDirectory)}`
            : "Skill"
        }
      >
        {skill.description}
      </ChipCard>
    );
  const command = commands.find((c) => c.id === chip.id);
  return command ? (
    <ChipCard title={chip.label} subtitle="Command">
      {command.description}
    </ChipCard>
  ) : null;
}

const skillChip = (id: string): ComposerChip => ({
  kind: "command",
  id,
  label: `/${id}`,
});

/** A sent message's text with the skills its request selected drawn as
    chips: where the text names one, and before the text for the ones gents
    took from the opening lines, which it strips. */
export function SentText({
  content,
  skills,
}: {
  content: string;
  skills: readonly string[] | null | undefined;
}) {
  const node = useSelectedNode();
  const known = node?.skills;
  const selected = useMemo(() => [...new Set(skills ?? [])], [skills]);
  const kind = useMemo<ChipKind>(
    () => ({
      preview: (chip) => skillCard(chip, known ?? []),
      fromText: (text) =>
        [...text.matchAll(/(^|\s)\/([A-Za-z0-9_.-]+)(?=[\s.,;:!?)\]]|$)/g)]
          .filter((match) => selected.includes(match[2]!))
          .map((match) => ({
            index: match.index + match[1]!.length,
            length: match[2]!.length + 1,
            chip: skillChip(match[2]!),
          })),
    }),
    [known, selected],
  );
  if (!selected.length) return content;
  const parts = parseParts(content, { command: kind });
  const named = new Set(
    parts.flatMap((part) => (part.type === "chip" ? [part.chip.id] : [])),
  );
  const leading = selected.filter((id) => !named.has(id));
  return (
    <>
      {leading.map((id) => (
        <Chip key={`lead-${id}`} chip={skillChip(id)} kind={kind} />
      ))}
      {leading.length > 0 && content && " "}
      {parts.map((part, i) =>
        part.type === "text" ? (
          part.text
        ) : (
          <Chip key={i} chip={part.chip} kind={kind} />
        ),
      )}
    </>
  );
}
