import type { AgentContext, DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import { AreaRow, ChoiceRow, DraftActions, FactRow, TextRow } from "./editors";
import { fromLines, fromLinesOrNull, newId, toLines, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { DocumentSelection } from "./DocumentSelection";

function Editor({
  shell,
  deployment,
  context,
}: {
  shell: Shell;
  deployment: DeploymentView;
  context: AgentContext;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "contexts",
  };
  const saved = {
    displayName: context.display_name ?? "",
    description: context.description ?? "",
    systemPrompt: context.system_prompt ?? "",
    toolsId: context.tools_id ?? "",
    compactionId: context.compaction_id ?? "",
    skillIds: toLines(context.skill_ids ?? []),
    tags: toLines(context.tags ?? []),
  };
  const d = useDraft(saved, async (next) => {
    if (next.toolsId && !deployment.tools.some((row) => row.tools_id === next.toolsId))
      throw new Error("Choose an existing Tools document");
    if (
      next.compactionId &&
      !deployment.compactions.some((row) => row.compaction_id === next.compactionId)
    )
      throw new Error("Choose an existing compaction document");
    const skillIds = fromLines(next.skillIds);
    const missingSkill = skillIds.find(
      (skillId) => !deployment.skills.some((row) => row.skillId === skillId),
    );
    if (missingSkill) throw new Error(`Unknown skill ID: ${missingSkill}`);
    await shell.applyConfig((api) =>
      api.patchConfigComponents({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: "AgentContext",
            id: context.context_id,
            changes: {
              display_name: next.displayName || null,
              description: next.description || null,
              system_prompt: next.systemPrompt || null,
              tools_id: next.toolsId || null,
              compaction_id: next.compactionId || null,
              skill_ids: skillIds.length ? skillIds : null,
              tags: fromLinesOrNull(next.tags),
            },
          },
        ],
      }),
    );
  });
  const id = (f: string) => `${context.context_id}-${f}`;
  return (
    <>
      <Group title={context.display_name ?? context.context_id}>
        <FactRow label="Context ID" mono>
          {context.context_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <AreaRow
          id={id("prompt")}
          label="System prompt"
          value={d.draft.systemPrompt}
          onChange={(v) => d.set("systemPrompt", v)}
          onCommit={d.commit}
          rows={8}
        />
        <ChoiceRow
          id={id("tools")}
          label="Tools"
          value={d.draft.toolsId}
          onChange={(v) => d.choose("toolsId", v)}
          items={[
            { value: "", label: "None" },
            ...deployment.tools.map((t) => ({
              value: t.tools_id,
              label: t.display_name ?? t.tools_id,
            })),
          ]}
        />
        <ChoiceRow
          id={id("compact")}
          label="Compaction"
          value={d.draft.compactionId}
          onChange={(v) => d.choose("compactionId", v)}
          items={[
            { value: "", label: "Runtime default" },
            ...deployment.compactions.map((c) => ({
              value: c.compaction_id,
              label: c.display_name ?? c.compaction_id,
            })),
          ]}
        />
        {d.draft.toolsId && (
          <div className="px-4 py-3 text-sm">
            <button
              type="button"
              className="underline"
              onClick={() =>
                navigate({ ...base, section: "tools", item: d.draft.toolsId })
              }
            >
              Configure selected tools
            </button>
            <p className="mt-1 text-muted-foreground">
              Tool permissions and subagent targets belong to the selected Tools
              document and are shared by contexts that use it.
            </p>
          </div>
        )}
        <DocumentSelection
          label="Skills"
          options={deployment.skills.map((skill) => ({
            value: skill.skillId,
            label: skill.displayName ?? skill.name ?? skill.skillId,
            description: skill.description,
          }))}
          selected={fromLines(d.draft.skillIds)}
          onChange={(values) => d.set("skillIds", toLines(values))}
        />
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={3}
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
        label={context.display_name ?? context.context_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteContextConfig({
              contextId: context.context_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function ContextsPanel({
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
    section: "contexts",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.contexts.map((c) => ({
        id: c.context_id,
        title: c.display_name ?? c.context_id,
        meta: c.tools_id ?? "no tools",
      }))}
      createLabel="New context"
      empty="No contexts. A context is the prompt, tools and skills a behaviour runs with."
      onCreate={async () => {
        const context_id = newId("ctx");
        await shell.applyConfig((api) =>
          api.applyConfigComponents({
            document: {
              agent_principal: { agent_did: deployment.agentDid },
              contexts: [
                {
                  context_id,
                  agent_did: deployment.agentDid,
                  display_name: "New context",
                  system_prompt: "",
                  tools_id: deployment.tools[0]?.tools_id ?? null,
                  compaction_id: deployment.compactions[0]?.compaction_id ?? null,
                  skill_ids: null,
                  tags: null,
                },
              ],
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "contexts",
          item: context_id,
        });
      }}
      detail={(id) => {
        const context = deployment.contexts.find((c) => c.context_id === id)!;
        return (
          <Editor
            key={context.context_id}
            shell={shell}
            deployment={deployment}
            context={context}
          />
        );
      }}
    />
  );
}
