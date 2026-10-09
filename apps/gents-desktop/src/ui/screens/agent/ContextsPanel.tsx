import type { NodeView } from "../../../hooks/fleetStore";
import { useEffect, useState } from "react";
import { dependentsWarning } from "./dependents";
import { ArrowLeft } from "lucide-react";
import type { AgentContext } from "@source-inc/gents-desktop-client";
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
import { agentOf } from "@/lib/agents";
import { useApp } from "@/app/AppContext";

/* a context's detail names this many of its agents, then counts the rest */
const USERS_SHOWN = 8;

function Editor({
  deployment,
  context,
  after,
}: {
  deployment: NodeView;
  context: AgentContext;
  after?: Route;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
  const d = useDraft(
    saved,
    async (next) => {
      const skillIds = next.skillIds;
      await changeConfig("patchConfigComponents", {
        nodeDid: deployment.nodeDid,
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
      });
    },
    {
      /* each reference names a document the node still lists */
      problems: (next) => {
        const missingSkill = next.skillIds.find(
          (skillId) => !deployment.skills.some((row) => row.skillId === skillId),
        );
        return {
          toolsId:
            next.toolsId &&
            !deployment.tools.some((row) => row.tools_id === next.toolsId)
              ? "Choose an existing Tools document"
              : undefined,
          compactionId:
            next.compactionId &&
            !deployment.compactions.some(
              (row) => row.compaction_id === next.compactionId,
            )
              ? "Choose an existing compaction document"
              : undefined,
          skillIds: missingSkill ? `Unknown skill ID: ${missingSkill}` : undefined,
        };
      },
    },
  );
  const id = (f: string) => `${context.context_id}-${f}`;
  const users = deployment.agents.filter((b) => b.contextId === context.context_id);
  /* New tools… from the field, and the chosen document's editor beside the page */
  const [newTools, setNewTools] = useState<((id: string | null) => void) | null>(null);
  const [besideTools, setBesideTools] = useState<string | null>(null);
  const besideDoc = deployment.tools.find((t) => t.tools_id === besideTools) ?? null;
  return (
    <>
      <ToolsSheet
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
              : "No agent runs with it."
          }
        >
          {users.length ? (
            <span className="flex flex-wrap justify-end gap-x-3 gap-y-1">
              {users.slice(0, USERS_SHOWN).map((b) => (
                <a
                  key={b.agentId}
                  href={href({ ...base, section: "agents", item: b.agentId })}
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
        />
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          rows={2}
        />
        <AreaRow
          id={id("prompt")}
          label="System prompt"
          value={d.draft.systemPrompt}
          onChange={(v) => d.set("systemPrompt", v)}
          rows={6}
          stacked
        />
        <RefRow
          id={id("tools")}
          label="Tools"
          description="What every agent on this context may touch."
          value={d.draft.toolsId}
          error={d.problems.toolsId}
          onChange={(v) => d.set("toolsId", v)}
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
          error={d.problems.compactionId}
          onChange={(v) => d.set("compactionId", v)}
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
          description="None means the agent runs without skills."
          value={d.draft.skillIds}
          error={d.problems.skillIds}
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
        draft={d}
        fields={{
          toolsId: id("tools"),
          compactionId: id("compact"),
          skillIds: id("skills"),
        }}
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
              : `${users.length} agents use it and will be left without a context.`
        }
        onDelete={() =>
          changeConfig("deleteContextConfig", {
            contextId: context.context_id,
            nodeDid: deployment.nodeDid,
          })
        }
      />
    </>
  );
}

export function ContextsPanel({
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
    section: "contexts",
  };
  /* a context opened from its agent goes back to that agent */
  const originId = item ? contextOrigin(item) : null;
  const origin = agentOf(deployment, originId);
  const back = origin
    ? {
        route: { ...base, section: "agents", item: origin.agentId },
        label: `Back to ${origin.displayName}`,
      }
    : undefined;
  useEffect(() => {
    if (!item) forgetContextOrigins();
  }, [item]);
  /* reached from the Agents list to clear out unused ones, so they come first */
  const inUse = new Set(deployment.agents.map((b) => b.contextId));
  const unusedFirst = (a: AgentContext, b: AgentContext) =>
    Number(inUse.has(a.context_id)) - Number(inUse.has(b.context_id));
  return (
    <>
      {!item && (
        <div className="mb-6">
          <a
            href={href({ ...base, section: "agents" })}
            className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Agents
          </a>
          <p className="mt-3 max-w-prose text-sm text-muted-foreground">
            Each agent keeps its instructions and tools in a context. Contexts no agent
            uses can be deleted.
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
            const names = deployment.agents
              .filter((b) => b.contextId === c.context_id)
              .map((b) => b.displayName);
            return names.length
              ? `Used by ${names.length <= 3 ? names.join(", ") : `${names.slice(0, 2).join(", ")} and ${names.length - 2} more`}`
              : "Not used by any agent";
          })(),
          badge: deployment.agents.some((b) => b.contextId === c.context_id)
            ? undefined
            : "unused",
          trailing: (
            <RowMenu
              name={c.display_name ?? c.context_id}
              base={base}
              id={c.context_id}
              onDelete={() =>
                changeConfig("deleteContextConfig", {
                  contextId: c.context_id,
                  nodeDid: deployment.nodeDid,
                })
              }
              warning={dependentsWarning(deployment, "context", c.context_id)}
            />
          ),
        }))}
        createLabel="New context"
        empty="No contexts. Agents keep their instructions and tools in one."
        detail={(id) => {
          const context = deployment.contexts.find((c) => c.context_id === id)!;
          return (
            <Editor
              key={context.context_id}

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
