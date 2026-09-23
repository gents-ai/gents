/* Skills, as the desktop app's Skills tab: id (immutable after create),
   name, enabled, display name, description, instructions and tool
   dependencies, saved through SkillSaveRequest. */
import { dependentsWarning } from "./dependents";
import type { DeploymentView, SkillView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  DraftActions,
  FactRow,
  SwitchRow,
  TextRow,
  TagsRow,
  PathRow,
} from "./editors";
import { useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { newId } from "./draft";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";

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
    sourceDirectory: skill.sourceDirectory ?? "",
    toolRefs: skill.toolRefs,
    interfaceJson: skill.interfaceJson ?? "",
    tags: skill.tags,
  };
  const d = useDraft(saved, async (next) => {
    if (!next.name.trim()) throw new Error("Name is required");
    if (next.interfaceJson.trim()) {
      try {
        JSON.parse(next.interfaceJson);
      } catch {
        throw new Error("Interface JSON must be valid JSON");
      }
    }
    await shell.applyConfig((api) =>
      api.saveSkillConfig({
        document: {
          skill_id: skill.skillId,
          agent_did: deployment.agentDid,
          name: next.name.trim(),
          description: next.description || null,
          instructions: next.instructions,
          source_directory: next.sourceDirectory || null,
          tool_refs: next.toolRefs.length ? next.toolRefs : null,
          display_name: next.displayName || null,
          interface_json: next.interfaceJson || null,
          enabled: next.enabled,
          created_at: skill.createdAt,
          tags: next.tags.length ? next.tags : null,
        },
      }),
    );
  });
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
        <PathRow
          id={id("sourceDirectory")}
          label="Source directory"
          description="Where the skill's files live."
          value={d.draft.sourceDirectory}
          onChange={(v) => d.set("sourceDirectory", v)}
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
          stacked
        />
        <TagsRow
          id={id("tools")}
          label="Tool dependencies"
          description="Intersected with the behavior ceiling."
          value={d.draft.toolRefs}
          onChange={(v) => d.set("toolRefs", v)}
          placeholder="Add a tool"
        />
        <AreaRow
          id={id("interface")}
          label="Interface JSON"
          description="Optional structured interface metadata. Must be valid JSON."
          value={d.draft.interfaceJson}
          onChange={(v) => d.set("interfaceJson", v)}
          onCommit={d.commit}
          rows={4}
          mono
          stacked
        />
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <DeleteButton
        label={skill.name ?? skill.skillId}
        warning={dependentsWarning(deployment, "skill", skill.skillId)}
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

/* the canonical document for a skill view, for row edits and copies */
function skillDocument(deployment: DeploymentView, s: SkillView) {
  return {
    skill_id: s.skillId,
    agent_did: deployment.agentDid,
    name: s.name ?? undefined,
    description: s.description,
    instructions: s.instructions ?? "",
    source_directory: s.sourceDirectory,
    tool_refs: s.toolRefs.length ? s.toolRefs : null,
    display_name: s.displayName,
    interface_json: s.interfaceJson,
    enabled: s.enabled ?? true,
    created_at: s.createdAt,
    tags: s.tags.length ? s.tags : null,
  };
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
        meta: `${s.toolRefs.length} ${s.toolRefs.length === 1 ? "tool" : "tools"}${s.enabled === false ? " · disabled" : ""}`,
        tags: s.tags,
        trailing: (
          <RowMenu
            name={s.displayName ?? s.name ?? s.skillId}
            base={base}
            id={s.skillId}
            enabled={{
              checked: s.enabled !== false,
              onChange: (enabled) =>
                shell.applyConfig((api) =>
                  api.saveSkillConfig({
                    document: { ...skillDocument(deployment, s), enabled },
                  }),
                ),
            }}
            onDuplicate={async () => {
              const skill_id = newId("skill");
              await shell.applyConfig((api) =>
                api.saveSkillConfig({
                  document: {
                    ...skillDocument(deployment, s),
                    skill_id,
                    name: `${s.name ?? s.skillId}-copy`,
                    display_name: `${s.displayName ?? s.name ?? s.skillId} copy`,
                    created_at: null,
                  },
                }),
              );
              return skill_id;
            }}
            onDelete={() =>
              shell.applyConfig((api) =>
                api.deleteSkillConfig({
                  skillId: s.skillId,
                  agentDid: deployment.agentDid,
                }),
              )
            }
            warning={dependentsWarning(deployment, "skill", s.skillId)}
          />
        ),
      }))}
      createLabel="New skill"
      empty="No skills yet. A skill is instructions and tool references a behavior can load by name."
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
              tool_refs: null,
              display_name: null,
              enabled: false,
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
            key={skill.skillId}
            shell={shell}
            deployment={deployment}
            skill={skill}
          />
        );
      }}
    />
  );
}
