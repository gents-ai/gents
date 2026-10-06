/* A behavior as its editor drafts it: the fields, the context choice
   (an existing one, a duplicate or an empty one), what makes a draft
   unsavable, and a new behavior's starting draft. */
import type { NodeView } from "../../../hooks/fleetStore";
import type { AgentContext, BehaviorView } from "@source-inc/gents-desktop-client";
import { newId } from "./draft";
import { defaultAgentOf } from "@/lib/agents";

/* picker values for a context that does not exist until Save */
export const DUPLICATE = "new:duplicate";

export const EMPTY = "new:empty";

export const isNew = (choice: string) => choice === DUPLICATE || choice === EMPTY;

export type Draft = {
  displayName: string;
  description: string;
  tags: string[];
  /* an existing context_id, '' for none, or DUPLICATE / EMPTY */
  contextChoice: string;
  /* for a new context: its name, and the context it copies */
  contextName: string;
  copiedFrom: string;
  systemPrompt: string;
  toolsId: string;
  compactionId: string;
  skillIds: string[];
  inferenceProfileId: string;
};

/* what a save should also do: make the change its own copy */
export type SaveIntent = { copy?: boolean };

export const contextFields = (c: AgentContext | null) => ({
  systemPrompt: c?.system_prompt ?? "",
  toolsId: c?.tools_id ?? "",
  compactionId: c?.compaction_id ?? "",
  skillIds: c?.skill_ids ?? [],
});

export const nameOf = (c: AgentContext) => c.display_name ?? c.context_id;

/* the behaviors that point at a context, as saved */
export const usersOf = (deployment: NodeView, contextId: string) =>
  deployment.behaviors.filter((b) => b.contextId === contextId);

export const listNames = (names: string[]) =>
  names.length <= 2 ? names.join(" and ") : `${names[0]} and ${names.length - 1} more`;

/* a context name no other context has */
export function freshName(deployment: NodeView, base: string) {
  const taken = new Set(deployment.contexts.map((c) => c.display_name));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base} ${n}`)) return `${base} ${n}`;
}

/* what stops a save, keyed by the field that shows it */
export function problems(deployment: NodeView, next: Draft, draft = false) {
  const out: Partial<Record<keyof Draft, string>> = {};
  if (!next.displayName.trim()) out.displayName = "Give the behavior a name.";
  if (!next.contextChoice)
    out.contextChoice =
      "Choose a context or start one. Without one the behavior has no instructions and no tools.";
  else if (
    !isNew(next.contextChoice) &&
    !deployment.contexts.some((c) => c.context_id === next.contextChoice)
  )
    out.contextChoice = "That context no longer exists. Choose another.";
  /* a draft's context is named after the behavior at save */
  if (!draft && isNew(next.contextChoice) && !next.contextName.trim())
    out.contextName = "Name the new context.";
  if (!next.inferenceProfileId)
    out.inferenceProfileId = "Choose the inference profile it runs on.";
  else if (
    !deployment.inferenceProfiles.some((p) => p.profile_id === next.inferenceProfileId)
  )
    out.inferenceProfileId = "That profile no longer exists. Choose another.";
  if (next.contextChoice && !out.contextChoice) {
    if (next.toolsId && !deployment.tools.some((t) => t.tools_id === next.toolsId))
      out.toolsId = "That Tools document no longer exists.";
    if (
      next.compactionId &&
      !deployment.compactions.some((c) => c.compaction_id === next.compactionId)
    )
      out.compactionId = "That compaction no longer exists.";
    const unknown = next.skillIds.filter(
      (s) => !deployment.skills.some((k) => k.skillId === s),
    );
    if (unknown.length)
      out.skillIds = `Unknown ${unknown.length === 1 ? "skill" : "skills"}: ${unknown.join(", ")}`;
  }
  return out;
}

/* the order the fields appear in, for focusing the first problem */
export const FIELD_ORDER: [keyof Draft, string][] = [
  ["displayName", "name"],
  ["contextChoice", "context"],
  ["contextName", "context-name"],
  ["toolsId", "tools"],
  ["skillIds", "skills"],
  ["compactionId", "compact"],
  ["inferenceProfileId", "profile"],
];

/* the behavior a draft starts from: nothing saved, the default model */
export function newBehaviorView(deployment: NodeView): BehaviorView {
  return {
    behaviorId: newId("behavior"),
    agentDid: deployment.agentDid,
    displayName: "",
    description: null,
    contextId: null,
    inferenceProfileId:
      defaultAgentOf(deployment)?.inferenceProfileId ??
      deployment.inferenceProfiles[0]?.profile_id ??
      null,
    enabled: false,
    isDefault: false,
    tags: [],
    createdAt: null,
  };
}
