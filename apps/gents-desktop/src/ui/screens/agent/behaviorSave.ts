/* What saving a behavior draft writes, and what it then says about the
   contexts: kept apart from the editor that shows the draft. */
import type { AgentContext, BehaviorView } from "@source-inc/gents-desktop-client";

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

export type WrittenBehavior = {
  /** a new context was created for the behavior */
  creating: boolean;
  contextId: string;
  /** the new context's name, when one was created */
  newName: string;
};

/**
 * Writes a behavior draft: a new context (a copy, an empty one, or a
 * duplicate) lands with the behavior in one apply; an edit to an existing
 * behavior and its existing context is one patch of the changed fields; a
 * new behavior on an edited existing context patches the context and saves
 * the behavior; anything else saves the behavior alone.
 */
export async function writeBehaviorDraft({
  changeConfig,
  deployment,
  behavior,
  next,
  asCopy,
  draftMode,
  newContextId,
}: {
  changeConfig: ShellActions["changeConfig"];
  deployment: NodeView;
  behavior: BehaviorView;
  next: Draft;
  /** save the context as a new copy rather than editing the one chosen */
  asCopy: boolean;
  draftMode: DraftMode | undefined;
  /** the id a new context takes; the same one again on a retry */
  newContextId: () => string;
}): Promise<WrittenBehavior> {
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
  const behaviorDocument = {
    behavior_id: behavior.behaviorId,
    agent_did: deployment.agentDid,
    display_name: next.displayName.trim(),
    description: next.description.trim() || null,
    context_id: contextId || null,
    inference_profile_id: next.inferenceProfileId,
    enabled: draftMode ? Boolean(draftMode.enabled) : behavior.enabled,
    tags: next.tags.length ? next.tags : null,
    created_at: behavior.createdAt,
  };
  /* only the context fields this edit changed, so a concurrent edit to
     another field of a shared context is not overwritten */
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
  /* a new context and the behavior that points at it land together or
     not at all: one component apply is one transaction */
  if (creating)
    await changeConfig("applyConfigComponents", {
      document: {
        agent_principal: { agent_did: deployment.agentDid },
        contexts: [
          {
            context_id: contextId,
            agent_did: deployment.agentDid,
            display_name: newName,
            description: null,
            ...fields,
            tags: null,
          },
        ],
        agent_behaviors: [behaviorDocument],
      },
    });
  else if (target && edited && !draftMode)
    /* an existing behavior and its existing context: one patch call, one
       transaction, changed fields only */
    await changeConfig("patchConfigComponents", {
      agentDid: deployment.agentDid,
      patches: [
        {
          collection: "AgentContext",
          id: target.context_id,
          changes: changedFields,
        },
        {
          collection: "AgentBehavior",
          id: behavior.behaviorId,
          changes: {
            display_name: behaviorDocument.display_name,
            description: behaviorDocument.description,
            context_id: behaviorDocument.context_id,
            inference_profile_id: behaviorDocument.inference_profile_id,
            tags: behaviorDocument.tags,
          },
        },
      ],
    });
  else if (target && edited) {
    /* a new behavior on an edited existing context: the behavior has no
       document to patch yet, so the context is patched with it applied */
    await changeConfig("patchConfigComponents", {
      agentDid: deployment.agentDid,
      patches: [
        {
          collection: "AgentContext",
          id: target.context_id,
          changes: changedFields,
        },
      ],
    });
    await changeConfig("saveBehaviorConfig", { document: behaviorDocument });
  } else await changeConfig("saveBehaviorConfig", { document: behaviorDocument });
  return { creating, contextId, newName };
}

/**
 * What a save did to the contexts, said after it: a context created, and
 * whether the one the behavior left is still used by others; one left
 * unused is offered for deletion.
 */
export function contextOutcome(
  deployment: NodeView,
  behavior: BehaviorView,
  previous: AgentContext | null,
  { creating, contextId, newName }: WrittenBehavior,
): { said: string[]; unused: AgentContext | null } {
  const said: string[] = [];
  if (creating) said.push(`Created ${newName}.`);
  const left =
    previous && previous.context_id !== contextId
      ? usersOf(deployment, previous.context_id).filter(
          (b) => b.behaviorId !== behavior.behaviorId,
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
