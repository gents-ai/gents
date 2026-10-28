import { useMemo, useRef, type ReactNode } from "react";
import { BookOpen, Target } from "lucide-react";
import {
  ChipCard,
  type ChipKind,
  type ComposerHandle,
  type ComposerTrigger,
} from "@gents/ui/conversation";
import type { AgentContext, SkillView } from "@source-inc/gents-desktop-client";
import { effectiveContextSkills } from "@source-inc/gents-desktop-chat";
import { noteSlash, rankSlash, recentSlash } from "../lib/slashRecent";
import { chipSkills, withSkillLines } from "../lib/skillLines";

export type SlashCommand = { id: "goal"; description: string; disabled?: boolean };

export type SlashItem =
  { kind: "command"; command: SlashCommand } | { kind: "skill"; skill: SkillView };

type SlashRow = {
  key: string;
  name: string;
  also?: string;
  label: string;
  detail?: string;
  icon: ReactNode;
  group?: string;
  disabled?: boolean;
  item: SlashItem;
};

const folderName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/** The menu a slash opens in the composer: commands, which only begin a
    message, then the agent's skills, the last few used first, anywhere a
    word starts. A skill picked becomes a chip, and its card says what it
    does. */
export function useSlashMenu(
  skills: SkillView[],
  context: AgentContext | null | undefined,
  available: readonly SlashCommand[],
  onCommand: (command: SlashCommand["id"]) => void,
) {
  const offered = useMemo(
    () =>
      effectiveContextSkills(skills, context).filter(
        (skill) => skill.enabled !== false,
      ),
    [skills, context],
  );
  const trigger: ComposerTrigger<SlashRow> = {
    char: "/",
    at: "word",
    label: "Commands and skills",
    empty: (query) => `No commands or skills match /${query}`,
    search: (query, where) =>
      rankSlash(
        [
          /* a command acts at once, so it only starts a message */
          ...(where.opening ? available : []).map((command) => ({
            key: `command:${command.id}`,
            name: command.id,
            label: `/${command.id}`,
            detail: command.description,
            icon: <Target />,
            group: "Commands",
            disabled: command.disabled,
            item: { kind: "command" as const, command },
          })),
          ...offered.map((skill) => ({
            key: `skill:${skill.skillId}`,
            name: skill.skillId,
            also: [skill.displayName, skill.name].filter(Boolean).join(" "),
            label: `/${skill.skillId}`,
            detail: skill.description ?? skill.displayName ?? skill.name ?? undefined,
            icon: <BookOpen />,
            group: "Skills",
            item: { kind: "skill" as const, skill },
          })),
        ],
        query,
        recentSlash(),
      ),
    select: ({ key, item }) => {
      noteSlash(key);
      if (item.kind === "command") {
        onCommand(item.command.id);
        return null;
      }
      return {
        kind: "command",
        id: item.skill.skillId,
        label: `/${item.skill.skillId}`,
      };
    },
  };
  /* the card over a command chip, whether picked from the menu or typed out */
  const chipKinds = useMemo<Record<string, ChipKind>>(
    () => ({
      command: {
        preview: (chip) => {
          const skill = offered.find((s) => s.skillId === chip.id);
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
          const command = available.find((c) => c.id === chip.id);
          return command ? (
            <ChipCard title={chip.label} subtitle="Command">
              {command.description}
            </ChipCard>
          ) : null;
        },
      },
    }),
    [offered, available],
  );
  const names = useMemo(() => offered.map((skill) => skill.skillId), [offered]);
  const field = useRef<ComposerHandle>(null);
  return {
    trigger,
    chipKinds,
    /** the composer's field, whose chips say which skills run */
    field,
    /** the text to send: each skill chip runs, wherever it sits */
    prepare: (text: string) =>
      withSkillLines(text, chipSkills(field.current?.parts() ?? [], names)),
    /** the skills a slash can name, which the field draws as chips */
    names,
  };
}
