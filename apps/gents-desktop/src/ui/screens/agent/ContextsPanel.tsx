import { useEffect, useState } from "react";
import { ArrowLeft } from "lucide-react";
import type { AgentContext, DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { href, type Route } from "@/lib/router";
import {
  AreaRow,
  ChipsRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  RefRow,
  TagsRow,
  TextRow,
} from "./editors";
import { ToolsSheet } from "./ToolsSheet";
import { EditorSheet } from "./EditorSheet";
import { ToolsEditor } from "./ToolsPanel";
import { toolsInWords } from "./automation";
import { useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { contextOrigin, forgetContextOrigins } from "./contextOrigin";
import { RowMenu } from "./RowMenu";

/* a context's detail names this many of its behaviors, then counts the rest */
const USERS_SHOWN = 8;

function Editor({
  shell,
  deployment,
  context,
  after,
}: {
  shell: Shell;
  deployment: DeploymentView;
  context: AgentContext;
  after?: Route;
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
    skillIds: context.skill_ids ?? [],
    tags: context.tags ?? [],
  };
  const d = useDraft(saved, async (next) => {
    if (next.toolsId && !deployment.tools.some((row) => row.tools_id === next.toolsId))
      throw new Error("Choose an existing Tools document");
    if (
      next.compactionId &&
      !deployment.compactions.some((row) => row.compaction_id === next.compactionId)
    )
      throw new Error("Choose an existing compaction document");
    const skillIds = next.skillIds;
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
              tags: next.tags.length ? next.tags : null,
            },
          },
        ],
      }),
    );
  });
  const id = (f: string) => `${context.context_id}-${f}`;
  const users = deployment.behaviors.filter((b) => b.contextId === context.context_id);
  /* New tools… from the field, and the chosen document's editor beside the page */
  const [newTools, setNewTools] = useState<((id: string | null) => void) | null>(null);
  const [besideTools, setBesideTools] = useState<string | null>(null);
  const besideDoc = deployment.tools.find((t) => t.tools_id === besideTools) ?? null;
  return (
    <>
      <ToolsSheet
        shell={shell}
        deployment={deployment}
        open={newTools !== null}
        onClose={(toolsId) => {
          newTools?.(toolsId);
          setNewTools(null);
        }}
      />
      <EditorSheet
        open={besideDoc !== null}
        onClose={() => setBesideTools(null)}
        title={besideDoc?.display_name ?? besideDoc?.tools_id ?? "Tools"}
        description={besideDoc ? toolsInWords(besideDoc) : undefined}
        page={
          besideDoc
            ? { ...base, section: "tools", item: besideDoc.tools_id }
            : undefined
        }
      >
        {besideDoc && (
          <ToolsEditor
            key={besideDoc.tools_id}
            shell={shell}
            deployment={deployment}
            tools={besideDoc}
            embedded
          />
        )}
      </EditorSheet>
      <Group title={context.display_name ?? context.context_id}>
        <FactRow label="Context ID" mono>
          {context.context_id}
        </FactRow>
        <FactRow
          label="Used by"
          description={
            users.length
              ? "Edits here apply to every one of them."
              : "No behavior runs with it."
          }
        >
          {users.length ? (
            <span className="flex flex-wrap justify-end gap-x-3 gap-y-1">
              {users.slice(0, USERS_SHOWN).map((b) => (
                <a
                  key={b.behaviorId}
                  href={href({ ...base, section: "behaviors", item: b.behaviorId })}
                  className="underline-offset-2 hover:text-foreground hover:underline"
                >
                  {b.displayName}
                </a>
              ))}
              {users.length > USERS_SHOWN && (
                <span>and {users.length - USERS_SHOWN} more</span>
              )}
            </span>
          ) : (
            "—"
          )}
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
          rows={6}
          stacked
        />
        <RefRow
          id={id("tools")}
          label="Tools"
          description="What every behavior on this context may touch."
          value={d.draft.toolsId}
          onChange={(v) => d.choose("toolsId", v)}
          none="None"
          items={deployment.tools.map((t) => ({
            value: t.tools_id,
            label: `${t.display_name ?? t.tools_id} · ${toolsInWords(t)}`,
          }))}
          createLabel="New tools…"
          onCreate={() =>
            new Promise<string | null>((resolve) => {
              setNewTools(() => resolve);
            })
          }
          onOpen={(toolsId) => setBesideTools(toolsId)}
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
        <ChipsRow
          id={id("skills")}
          label="Skills"
          description="None means the behavior runs without skills."
          value={d.draft.skillIds}
          onChange={(v) => d.set("skillIds", v)}
          items={deployment.skills.map((sk) => ({
            value: sk.skillId,
            label: sk.displayName ?? sk.name ?? sk.skillId,
          }))}
          placeholder="Add a skill"
          empty="No skill by that name."
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
        label={context.display_name ?? context.context_id}
        base={base}
        after={after}
        warning={
          users.length === 0
            ? undefined
            : users.length === 1
              ? `${users[0]!.displayName} uses it and will be left without a context.`
              : `${users.length} behaviors use it and will be left without a context.`
        }
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
  /* a context opened from its behavior goes back to that behavior */
  const originId = item ? contextOrigin(item) : null;
  const origin = deployment.behaviors.find((b) => b.behaviorId === originId);
  const back = origin
    ? {
        route: { ...base, section: "behaviors", item: origin.behaviorId },
        label: `Back to ${origin.displayName}`,
      }
    : undefined;
  useEffect(() => {
    if (!item) forgetContextOrigins();
  }, [item]);
  /* reached from the Behaviors list to clear out unused ones, so they come first */
  const inUse = new Set(deployment.behaviors.map((b) => b.contextId));
  const unusedFirst = (a: AgentContext, b: AgentContext) =>
    Number(inUse.has(a.context_id)) - Number(inUse.has(b.context_id));
  return (
    <>
      {!item && (
        <div className="mb-6">
          <a
            href={href({ ...base, section: "behaviors" })}
            className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Behaviors
          </a>
          <p className="mt-3 max-w-prose text-sm text-muted-foreground">
            Each behavior keeps its instructions and tools in a context. Contexts no
            behavior uses can be deleted.
          </p>
        </div>
      )}
      <ListDetail
        base={base}
        item={item}
        back={back}
        rows={[...deployment.contexts].sort(unusedFirst).map((c) => ({
          id: c.context_id,
          title: c.display_name ?? c.context_id,
          tags: c.tags,
          meta: (() => {
            const names = deployment.behaviors
              .filter((b) => b.contextId === c.context_id)
              .map((b) => b.displayName);
            return names.length
              ? `Used by ${names.length <= 3 ? names.join(", ") : `${names.slice(0, 2).join(", ")} and ${names.length - 2} more`}`
              : "Not used by any behavior";
          })(),
          badge: deployment.behaviors.some((b) => b.contextId === c.context_id)
            ? undefined
            : "unused",
          trailing: (
            <RowMenu
              name={c.display_name ?? c.context_id}
              base={base}
              id={c.context_id}
              onDelete={() =>
                shell.applyConfig((api) =>
                  api.deleteContextConfig({
                    contextId: c.context_id,
                    agentDid: deployment.agentDid,
                  }),
                )
              }
              warning={(() => {
                const n = deployment.behaviors.filter(
                  (b) => b.contextId === c.context_id,
                ).length;
                return n
                  ? `${n} ${n === 1 ? "behavior loses" : "behaviors lose"} its instructions.`
                  : undefined;
              })()}
            />
          ),
        }))}
        createLabel="New context"
        empty="No contexts. Behaviors keep their instructions and tools in one."
        detail={(id) => {
          const context = deployment.contexts.find((c) => c.context_id === id)!;
          return (
            <Editor
              key={context.context_id}
              shell={shell}
              deployment={deployment}
              context={context}
              after={back?.route}
            />
          );
        }}
      />
    </>
  );
}
