import type { AgentContext, AgentView } from "@source-inc/gents-desktop-client";

import type { NodeView } from "../../../hooks/fleetStore";
import type { ShellActions } from "@/../hooks/shellActions";
import {
  contextFields,
  freshName,
  isNew,
  listNames,
  nameOf,
  usersOf,
  type Draft,
  type DraftMode,
} from "./behaviorDraft";

export type WrittenAgent = {
  creating: boolean;
  contextId: string;
  newName: string;
};

export async function writeAgentDraft({
  changeConfig,
  deployment,
  agent,
  next,
  asCopy,
  draftMode,
  newContextId,
}: {
  changeConfig: ShellActions["changeConfig"];
  deployment: NodeView;
  agent: AgentView;
  next: Draft;
  asCopy: boolean;
  draftMode: DraftMode | undefined;
  newContextId: () => string;
}): Promise<WrittenAgent> {
  const creating = isNew(next.contextChoice) || asCopy;
  const newName =
    next.contextName.trim() ||
    freshName(deployment, `${next.displayName.trim() || "New"} context`);
  const target = creating
    ? null
    : (deployment.contexts.find((c) => c.context_id === next.contextChoice) ?? null);
  const contextId = creating ? newContextId() : next.contextChoice;
  const fields = {
    system_prompt: next.systemPrompt || null,
    tools_id: next.toolsId || null,
    compaction_id: next.compactionId || null,
    skill_ids: next.skillIds.length ? next.skillIds : null,
  };
  const edited =
    target !== null &&
    JSON.stringify(contextFields(target)) !==
      JSON.stringify({
        systemPrompt: next.systemPrompt,
        toolsId: next.toolsId,
        compactionId: next.compactionId,
        skillIds: next.skillIds,
      });
  const agentDocument = {
    agent_id: agent.agentId,
    node_did: deployment.nodeDid,
    display_name: next.displayName.trim(),
    description: next.description.trim() || null,
    context_id: contextId || null,
    inference_profile_id: next.inferenceProfileId,
    enabled: draftMode ? Boolean(draftMode.enabled) : agent.enabled,
    tags: next.tags.length ? next.tags : null,
    created_at: agent.createdAt,
  };
  const before = target ? contextFields(target) : null;
  const changedFields: Partial<typeof fields> = {};
  if (before) {
    if (before.systemPrompt !== next.systemPrompt)
      changedFields.system_prompt = fields.system_prompt;
    if (before.toolsId !== next.toolsId) changedFields.tools_id = fields.tools_id;
    if (before.compactionId !== next.compactionId)
      changedFields.compaction_id = fields.compaction_id;
    if (JSON.stringify(before.skillIds) !== JSON.stringify(next.skillIds))
      changedFields.skill_ids = fields.skill_ids;
  }
  if (creating)
    await changeConfig("applyConfigComponents", {
      document: {
        node: { node_did: deployment.nodeDid },
        contexts: [
          {
            context_id: contextId,
            node_did: deployment.nodeDid,
            display_name: newName,
            description: null,
            ...fields,
            tags: null,
          },
        ],
        agents: [agentDocument],
      },
    });
  else if (target && edited && !draftMode)
    await changeConfig("patchConfigComponents", {
      nodeDid: deployment.nodeDid,
      patches: [
        {
          collection: "AgentContext",
          id: target.context_id,
          changes: changedFields,
        },
        {
          collection: "Agent",
          id: agent.agentId,
          changes: {
            display_name: agentDocument.display_name,
            description: agentDocument.description,
            context_id: agentDocument.context_id,
            inference_profile_id: agentDocument.inference_profile_id,
            tags: agentDocument.tags,
          },
        },
      ],
    });
  else if (target && edited) {
    await changeConfig("patchConfigComponents", {
      nodeDid: deployment.nodeDid,
      patches: [
        {
          collection: "AgentContext",
          id: target.context_id,
          changes: changedFields,
        },
      ],
    });
    await changeConfig("saveAgentConfig", { document: agentDocument });
  } else await changeConfig("saveAgentConfig", { document: agentDocument });
  return { creating, contextId, newName };
}

export function contextOutcome(
  deployment: NodeView,
  agent: AgentView,
  previous: AgentContext | null,
  { creating, contextId, newName }: WrittenAgent,
): { said: string[]; unused: AgentContext | null } {
  const said: string[] = [];
  if (creating) said.push(`Created ${newName}.`);
  const left =
    previous && previous.context_id !== contextId
      ? usersOf(deployment, previous.context_id).filter(
          (b) => b.agentId !== agent.agentId,
        )
      : null;
  if (previous && left) {
    said.push(
      left.length
        ? `${nameOf(previous)} is still used by ${listNames(left.map((b) => b.displayName))}.`
        : `${nameOf(previous)} is no longer used.`,
    );
  }
  return { said, unused: previous && left && left.length === 0 ? previous : null };
}
