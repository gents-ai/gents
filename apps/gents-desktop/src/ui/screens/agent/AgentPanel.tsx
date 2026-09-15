/* The agent itself: editable principal fields and identity/runtime facts. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { isLocalAgent } from "@/lib/firstRun";
import { AreaRow, ChoiceRow, DraftActions, SwitchRow, TextRow } from "./editors";
import { fromLinesOrNull, toLines, useDraft } from "./draft";
import { Fact, Group, Row } from "./rows";
import { LocalServer } from "./LocalServer";

export function AgentPanel({
  shell,
  deployment,
}: {
  shell: Shell;
  deployment: DeploymentView;
}) {
  const agent = deployment.agentPrincipal;
  const behaviours = deployment.behaviors.map((b) => ({
    value: b.behaviorId,
    label: b.displayName,
  }));
  const d = useDraft(
    {
      displayName: agent.displayName ?? "",
      defaultBehaviorId: agent.defaultBehaviorId ?? "",
      enabled: agent.enabled ?? true,
      tags: toLines(deployment.principalConfig?.tags ?? []),
    },
    async (next) => {
      if (!next.displayName.trim()) throw new Error("Display name is required");
      if (!next.defaultBehaviorId) throw new Error("Default behaviour is required");
      await shell.saveAgentConfig({
        document: {
          agent_did: agent.agentDid,
          display_name: next.displayName.trim(),
          default_behavior_id: next.defaultBehaviorId,
          enabled: next.enabled,
          created_at: agent.createdAt,
          created_by: agent.createdBy,
          tags: fromLinesOrNull(next.tags),
        },
      });
    },
  );

  return (
    <div>
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
        <ChoiceRow
          id="agent-default"
          label="Default behaviour"
          description="Used when a session does not choose one."
          value={d.draft.defaultBehaviorId}
          onChange={(v) => d.choose("defaultBehaviorId", v)}
          items={behaviours}
        />
        <SwitchRow
          id="agent-enabled"
          label="Enabled"
          description="A disabled agent accepts no requests."
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <AreaRow
          id="agent-tags"
          label="Tags"
          description="One optional discovery label per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={2}
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
          description="The most any behaviour on this agent may do."
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
        <Row label="Executors" description="Behaviour executors in use over capacity.">
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
