/* The agent itself: editable principal fields and identity/runtime facts. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { isLocalAgent } from "@/lib/firstRun";
import { DraftActions, RefRow, SwitchRow, TagsRow, TextRow } from "./editors";
import { useDraft } from "./draft";
import { Fact, Group, Row } from "./rows";
import { LocalServer } from "./LocalServer";
import { AgentCard } from "./AgentCard";
import { saveDefault } from "./BehaviorsPanel";

export function AgentPanel({
  shell,
  deployment,
}: {
  shell: Shell;
  deployment: DeploymentView;
}) {
  const agent = deployment.agentPrincipal;
  const behaviors = deployment.behaviors.map((b) => ({
    value: b.behaviorId,
    label: b.enabled
      ? b.displayName
      : `${b.displayName} · disabled, enabled when saved as default`,
  }));
  const d = useDraft(
    {
      displayName: agent.displayName ?? "",
      defaultBehaviorId: agent.defaultBehaviorId ?? "",
      enabled: agent.enabled ?? true,
      tags: deployment.principalConfig?.tags ?? [],
    },
    async (next) => {
      if (!next.displayName.trim()) throw new Error("Display name is required");
      if (!next.defaultBehaviorId) throw new Error("Default behavior is required");
      /* a new default lands with its enablement first; the principal's other
         fields then save against an already valid default */
      if (next.defaultBehaviorId !== agent.defaultBehaviorId)
        await saveDefault(shell, deployment, next.defaultBehaviorId);
      await shell.saveAgentConfig({
        document: {
          agent_did: agent.agentDid,
          display_name: next.displayName.trim(),
          default_behavior_id: next.defaultBehaviorId,
          enabled: next.enabled,
          created_at: agent.createdAt,
          created_by: agent.createdBy,
          tags: next.tags.length ? next.tags : null,
        },
      });
    },
  );

  return (
    <div>
      <AgentCard shell={shell} deployment={deployment} />
      <Group title="Agent details">
        <TextRow
          id="agent-name"
          label="Display name"
          description="How this agent is named across the desktop."
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <RefRow
          id="agent-default"
          label="Default behavior"
          description="Used when a session does not choose one."
          value={d.draft.defaultBehaviorId}
          onChange={(v) => d.choose("defaultBehaviorId", v)}
          items={behaviors}
          createLabel="New behavior…"
          openRoute={(behaviorId) => ({
            name: "agent",
            agentDid: deployment.agentDid,
            section: "behaviors",
            item: behaviorId,
          })}
        />
        <SwitchRow
          id="agent-enabled"
          label="Enabled"
          description="A disabled agent accepts no requests."
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <TagsRow
          id="agent-tags"
          label="Tags"
          description="Optional discovery labels."
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

      <Group title="Identity">
        <Row
          label="Agent DID"
          description="The cryptographic principal. Permissions and audit are keyed by it."
        >
          <Fact mono>{agent.agentDid}</Fact>
        </Row>
        <Row label="Install name">
          <Fact>{shell.snapshot?.bootstrap.initAgentName}</Fact>
        </Row>
        <Row
          label="Tool ceiling"
          description="The most any behavior on this agent may do."
        >
          <Fact>{shell.snapshot?.bootstrap.initToolCeiling ?? "not configured"}</Fact>
        </Row>
        <Row label="Tool root" description="The directory tools are confined to.">
          <Fact mono>{shell.snapshot?.bootstrap.initToolRoot ?? "not configured"}</Fact>
        </Row>
        <Row label="Peer" description="Where the agent's node runs.">
          <Fact mono>{deployment.peerId}</Fact>
        </Row>
        <Row label="Created">
          <Fact>
            {agent.createdAt ? new Date(agent.createdAt).toLocaleDateString() : null}
          </Fact>
        </Row>
      </Group>

      <Group title="Runtime">
        <Row
          label="Reconcile"
          description="The last pass of the agent's runtime over its configuration."
        >
          <Fact>
            {deployment.runtime?.reconcilePhase} ·{" "}
            {deployment.runtime?.lastReconcileResult}
          </Fact>
        </Row>
        <Row label="Executors" description="Behavior executors in use over capacity.">
          <Fact>
            {deployment.runtime?.behaviorExecutorQueueDepth ?? 0} /{" "}
            {deployment.runtime?.behaviorExecutorCapacity ?? 0}
          </Fact>
        </Row>
      </Group>
      {isLocalAgent(deployment, shell.snapshot?.bootstrap.initAgentDid) && (
        <LocalServer shell={shell} />
      )}
    </div>
  );
}
