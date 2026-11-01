import type { NodeView } from "../../../hooks/fleetStore";
import { dependentsWarning } from "./dependents";
import type { SkillView } from "@source-inc/gents-desktop-client";
import { navigate } from "@/lib/router";
import { AreaRow, DraftActions, FactRow, TextRow, TagsRow, PathRow } from "./editors";
import { DeleteButton, ListDetail, packOrigin } from "./ListDetail";
import { newId, problemOf, useDraft } from "./draft";
import { focusField, Group, Unfold } from "./rows";
import { RowMenu } from "./RowMenu";
import { usersOf } from "./behaviorDraft";
import { useApp } from "@/app/AppContext";
import { href } from "@/lib/router";
import { cn } from "@gents/ui/lib/utils";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Switch } from "@gents/ui/components/switch";
import { Pencil } from "lucide-react";
import { Fragment, useState } from "react";
import { Markdown } from "../Markdown";

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
  const [editing, setEditing] = useState(false);
  const [writing, setWriting] = useState(false);
  const users = skillUsers(deployment, skill.skillId);
  const pack = packOrigin(skill.tags);
  const title = d.draft.displayName.trim() || d.draft.name.trim() || skill.skillId;
  return (
    <>
      <div className="mb-8 grid gap-1">
        <div className="flex items-start gap-4">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <h2
                className={cn(
                  "truncate font-heading text-lg font-medium",
                  d.draft.enabled ? "text-heading" : "text-muted-foreground",
                )}
              >
                {title}
              </h2>
              {!d.draft.enabled && <Badge variant="outline">Disabled</Badge>}
              <Button
                size="xs"
                variant="quiet"
                aria-expanded={editing}
                aria-controls={id("about")}
                onClick={() => setEditing((open) => !open)}
              >
                {editing ? (
                  "Done"
                ) : (
                  <>
                    <Pencil /> Edit
                  </>
                )}
              </Button>
            </div>
            {d.draft.description.trim() && (
              <p className="mt-0.5 text-sm text-foreground/80">{d.draft.description}</p>
            )}
            <p className="mt-2 text-sm text-muted-foreground">
              Used by{" "}
              {users.length ? (
                users.map((agent, i) => (
                  <Fragment key={agent.agentId}>
                    {i > 0 && ", "}
                    <a
                      href={href({
                        name: "agent",
                        nodeDid: deployment.nodeDid,
                        section: "agents",
                        item: agent.agentId,
                      })}
                      className="text-foreground underline-offset-2 hover:underline"
                    >
                      {agent.displayName}
                    </a>
                  </Fragment>
                ))
              ) : (
                <span className="text-foreground">no agent yet</span>
              )}
              {" · From "}
              <span className="text-foreground">{pack ?? "Your own"}</span>
            </p>
          </div>
          {/* switched here, saved with the rest of the form */}
          <label className="flex shrink-0 items-center gap-2.5 text-sm">
            {d.draft.enabled ? "Enabled" : "Disabled"}
            <Switch
              aria-label="Enabled"
              checked={d.draft.enabled}
              onCheckedChange={(v) => d.set("enabled", v)}
            />
          </label>
        </div>
      </div>
      <Unfold open={editing} id={id("about")}>
        <Group title="About">
          <TextRow
            id={id("name")}
            label="Name"
            value={d.draft.name}
            onChange={(v) => d.set("name", v)}
            error={d.problems.name}
          />
          <TextRow
            id={id("display")}
            label="Display name"
            value={d.draft.displayName}
            onChange={(v) => d.set("displayName", v)}
          />
          <AreaRow
            id={id("description")}
            label="Description"
            description="Shown in the skills catalog."
            value={d.draft.description}
            onChange={(v) => d.set("description", v)}
            rows={2}
          />
        </Group>
      </Unfold>
      <section aria-labelledby={id("instructions-title")} className="mb-8">
        <div className="mb-2 flex items-center gap-2">
          <h3
            id={id("instructions-title")}
            className="text-sm font-medium text-heading"
          >
            Instructions
          </h3>
          <Button
            size="xs"
            variant="quiet"
            aria-label={writing ? "Done editing instructions" : "Edit instructions"}
            aria-expanded={writing}
            onClick={() => {
              setWriting((open) => !open);
              if (!writing) focusField(id("instructions"));
            }}
          >
            {writing ? (
              "Done"
            ) : (
              <>
                <Pencil /> Edit
              </>
            )}
          </Button>
        </div>
        {writing ? (
          <Group>
            <AreaRow
              id={id("instructions")}
              label="Instructions"
              description="Loaded on demand via load_skill."
              value={d.draft.instructions}
              onChange={(v) => d.set("instructions", v)}
              rows={12}
              stacked
            />
          </Group>
        ) : d.draft.instructions.trim() ? (
          <div className="prose-app min-w-0 break-words">
            <Markdown>{d.draft.instructions}</Markdown>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No instructions yet. They're what an agent reads when it loads the skill.
          </p>
        )}
      </section>
      <Group title="Details">
        <FactRow label="Skill ID" description="Cannot be renamed after creation." mono>
          {skill.skillId}
        </FactRow>
        <PathRow
          id={id("sourceDirectory")}
          label="Source directory"
          description="Where the skill's files live."
          value={d.draft.sourceDirectory}
          onChange={(v) => d.set("sourceDirectory", v)}
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

/** agents whose instructions list the skill */
function skillUsers(deployment: NodeView, skillId: string) {
  return deployment.contexts
    .filter((c) => c.skill_ids?.includes(skillId))
    .flatMap((c) => usersOf(deployment, c.context_id));
}

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
