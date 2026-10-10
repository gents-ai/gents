/* Skills: id (immutable after create),
   name, enabled, display name, description, instructions and tool
   dependencies, saved through SkillSaveRequest. */
import type { NodeView } from "../../../hooks/fleetStore";
import { dependentsWarning } from "./dependents";
import type { SkillView } from "@source-inc/gents-desktop-client";
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
import { DeleteButton, ListDetail } from "./ListDetail";
import { newId, problemOf, useDraft } from "./draft";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";
import { useApp } from "@/app/AppContext";

function SkillEditor({
  deployment,
  skill,
}: {
  deployment: NodeView;
  skill: SkillView;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
  const d = useDraft(
    saved,
    async (next) =>
      changeConfig("saveSkillConfig", {
        document: {
          skill_id: skill.skillId,
          node_did: deployment.nodeDid,
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
    {
      problems: (next) => ({
        name: next.name.trim() ? undefined : "Name is required",
        interfaceJson:
          next.interfaceJson.trim() && problemOf(() => JSON.parse(next.interfaceJson))
            ? "Interface JSON must be valid JSON"
            : undefined,
      }),
    },
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
          error={d.problems.name}
        />
        <PathRow
          id={id("sourceDirectory")}
          label="Source directory"
          description="Where the skill's files live."
          value={d.draft.sourceDirectory}
          onChange={(v) => d.set("sourceDirectory", v)}
        />
        <TextRow
          id={id("display")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          description="Shown in the skills catalog."
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          rows={2}
        />
        <AreaRow
          id={id("instructions")}
          label="Instructions"
          description="Loaded on demand via load_skill."
          value={d.draft.instructions}
          onChange={(v) => d.set("instructions", v)}
          rows={6}
          stacked
        />
        <TagsRow
          id={id("tools")}
          label="Tool dependencies"
          description="Intersected with the agent's ceiling."
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
          error={d.problems.interfaceJson}
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
        draft={d}
        fields={{ name: id("name"), interfaceJson: id("interface") }}
      />
      <DeleteButton
        label={skill.name ?? skill.skillId}
        warning={dependentsWarning(deployment, "skill", skill.skillId)}
        base={base}
        onDelete={() =>
          changeConfig("deleteSkillConfig", {
            skillId: skill.skillId,
            nodeDid: deployment.nodeDid,
          })
        }
      />
    </>
  );
}

/* the canonical document for a skill view, for row edits and copies */
function skillDocument(deployment: NodeView, s: SkillView) {
  return {
    skill_id: s.skillId,
    node_did: deployment.nodeDid,
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
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
                changeConfig(
                  "saveSkillConfig",
                  {
                    document: { ...skillDocument(deployment, s), enabled },
                  },
                  `turn it ${enabled ? "on" : "off"}`,
                ),
            }}
            onDuplicate={async () => {
              const skill_id = newId("skill");
              await changeConfig("saveSkillConfig", {
                document: {
                  ...skillDocument(deployment, s),
                  skill_id,
                  name: `${s.name ?? s.skillId}-copy`,
                  display_name: `${s.displayName ?? s.name ?? s.skillId} copy`,
                  created_at: null,
                },
              });
              return skill_id;
            }}
            onDelete={() =>
              changeConfig("deleteSkillConfig", {
                skillId: s.skillId,
                nodeDid: deployment.nodeDid,
              })
            }
            warning={dependentsWarning(deployment, "skill", s.skillId)}
          />
        ),
      }))}
      createLabel="New skill"
      empty="No skills yet. A skill is instructions and tool references an agent can load by name."
      onCreate={async () => {
        const skillId = newId("skill");
        await changeConfig("saveSkillConfig", {
          document: {
            skill_id: skillId,
            node_did: deployment.nodeDid,
            name: "New skill",
            description: null,
            instructions: "",
            tool_refs: null,
            display_name: null,
            enabled: false,
          },
        });
        navigate({
          name: "agent",
          nodeDid: deployment.nodeDid,
          section: "skills",
          item: skillId,
        });
      }}
      detail={(id) => {
        const skill = deployment.skills.find((s) => s.skillId === id)!;
        return (
          <SkillEditor key={skill.skillId} deployment={deployment} skill={skill} />
        );
      }}
    />
  );
}
