/* Behaviours pick a context and an inference profile. Prompt, tools and
   skills live on AgentContext (see ContextsPanel). */
import type { DeploymentView, BehaviorView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { ChoiceRow, FactRow, SwitchRow, TextRow } from "./editors";
import { useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { createBehavior } from "./createBehavior";

function Editor({
  shell,
  deployment,
  behavior,
}: {
  shell: Shell;
  deployment: DeploymentView;
  behavior: BehaviorView;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "behaviors",
  };
  const saved = {
    displayName: behavior.displayName,
    description: behavior.description ?? "",
    contextId: behavior.contextId ?? "",
    inferenceProfileId: behavior.inferenceProfileId ?? "",
    enabled: behavior.enabled,
  };
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveBehaviorConfig({
        document: {
          behavior_id: behavior.behaviorId,
          agent_did: deployment.agentDid,
          display_name: next.displayName,
          description: next.description || null,
          context_id: next.contextId || null,
          inference_profile_id: next.inferenceProfileId,
          enabled: next.enabled,
          tags: behavior.tags,
          created_at: behavior.createdAt,
        },
      }),
    ),
  );
  const id = (f: string) => `${behavior.behaviorId}-${f}`;
  return (
    <>
      <Group title={behavior.displayName}>
        <FactRow label="Behaviour ID" mono>
          {behavior.behaviorId}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("context")}
          label="Context"
          description="System prompt, tools and skills."
          value={d.draft.contextId}
          onChange={(v) => d.choose("contextId", v)}
          items={deployment.contexts.map((c) => ({
            value: c.context_id,
            label: c.display_name ?? c.context_id,
          }))}
        />
        <ChoiceRow
          id={id("profile")}
          label="Inference profile"
          value={d.draft.inferenceProfileId}
          onChange={(v) => d.choose("inferenceProfileId", v)}
          items={deployment.inferenceProfiles.map((p) => ({
            value: p.profile_id,
            label: p.display_name ?? p.profile_id,
          }))}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
      </Group>
      <DeleteButton
        label={behavior.displayName}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteBehaviorConfig({
              behaviorId: behavior.behaviorId,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function BehaviorsPanel({
  shell,
  deployment,
  behaviorId,
}: {
  shell: Shell;
  deployment: DeploymentView;
  behaviorId?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "behaviors",
  };
  return (
    <ListDetail
      base={base}
      item={behaviorId}
      rows={deployment.behaviors.map((b) => ({
        id: b.behaviorId,
        title: b.displayName,
        meta: b.isDefault ? "default" : b.enabled ? "enabled" : "disabled",
      }))}
      createLabel="New behaviour"
      empty="No behaviours yet."
      onCreate={() => createBehavior(shell, deployment)}
      detail={(id) => {
        const behavior = deployment.behaviors.find((b) => b.behaviorId === id)!;
        return (
          <Editor
            key={JSON.stringify(behavior)}
            shell={shell}
            deployment={deployment}
            behavior={behavior}
          />
        );
      }}
    />
  );
}
