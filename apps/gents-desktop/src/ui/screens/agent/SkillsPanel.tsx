/* Skills, as the desktop app's Skills tab: id (immutable after create),
   name, scope, enabled, display name, description, instructions and tool
   dependencies, saved through SkillSaveRequest. */
import type { DeploymentView, SkillView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import { AreaRow, FactRow, SwitchRow, TextRow } from "./editors";
import { fromLines, toLines, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { newId } from "./draft";
import { Group } from "./rows";

function SkillEditor({
  shell,
  deployment,
  skill,
}: {
  shell: Shell;
  deployment: DeploymentView;
  skill: SkillView;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "skills",
  };
  const saved = {
    name: skill.name ?? "",
    enabled: skill.enabled ?? true,
    displayName: skill.displayName ?? "",
    description: skill.description ?? "",
    instructions: skill.instructions ?? "",
    toolRefs: toLines(skill.toolRefs),
  };
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveSkillConfig({
        document: {
          skill_id: skill.skillId,
          agent_did: deployment.agentDid,
          name: next.name.trim() || skill.skillId,
          description: next.description || null,
          instructions: next.instructions,
          tool_refs: fromLines(next.toolRefs),
          display_name: next.displayName || null,
          enabled: next.enabled,
          created_at: skill.createdAt,
          tags: null,
        },
      }),
    ),
  );
  const id = (f: string) => `${skill.skillId}-${f}`;
  return (
    <>
      <Group title={skill.displayName ?? skill.name ?? skill.skillId}>
        <FactRow label="Skill ID" description="Cannot be renamed after creation." mono>
          {skill.skillId}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Name"
          value={d.draft.name}
          onChange={(v) => d.set("name", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("display")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          description="Shown in the skills catalog."
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <AreaRow
          id={id("instructions")}
          label="Instructions"
          description="Loaded on demand via load_skill."
          value={d.draft.instructions}
          onChange={(v) => d.set("instructions", v)}
          onCommit={d.commit}
          rows={6}
        />
        <AreaRow
          id={id("tools")}
          label="Tool dependencies"
          description="One per line; intersected with the behaviour ceiling."
          value={d.draft.toolRefs}
          onChange={(v) => d.set("toolRefs", v)}
          onCommit={d.commit}
          rows={3}
          mono
        />
      </Group>
      <DeleteButton
        label={skill.name ?? skill.skillId}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteSkillConfig({
              skillId: skill.skillId,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function SkillsPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "skills",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.skills.map((s) => ({
        id: s.skillId,
        title: s.displayName ?? s.name ?? s.skillId,
        meta: `${s.toolRefs.length} tools${s.enabled === false ? " · disabled" : ""}`,
      }))}
      createLabel="New skill"
      empty="No skills yet. A skill is instructions and tool references a behaviour can load by name."
      onCreate={async () => {
        const skillId = newId("skill");
        await shell.applyConfig((api) =>
          api.saveSkillConfig({
            document: {
              skill_id: skillId,
              agent_did: deployment.agentDid,
              name: "New skill",
              description: null,
              instructions: "",
              tool_refs: [],
              display_name: null,
              enabled: true,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "skills",
          item: skillId,
        });
      }}
      detail={(id) => {
        const skill = deployment.skills.find((s) => s.skillId === id)!;
        return (
          <SkillEditor
            key={JSON.stringify(skill)}
            shell={shell}
            deployment={deployment}
            skill={skill}
          />
        );
      }}
    />
  );
}
