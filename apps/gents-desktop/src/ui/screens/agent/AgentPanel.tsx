/* The node itself: editable node fields and identity/runtime facts. */
import type { NodeView } from "../../../hooks/fleetStore";
import { isLocalNode } from "@/lib/firstRun";
import { DraftActions, RefRow, SwitchRow, TagsRow, TextRow } from "./editors";
import { useDraft } from "./draft";
import { Fact, Group, Row } from "./rows";
import { LocalServer } from "./LocalServer";
import { AgentCard } from "./AgentCard";
import { saveDefault } from "./BehaviorsPanel";
import { useApp } from "@/app/AppContext";
import { useBootstrap } from "@/hooks/useClient";

export function AgentPanel({ deployment }: { deployment: NodeView }) {
  const bootstrap = useBootstrap();
  const { changeConfig } = useApp().actions;
  const agent = deployment.node;
  const agents = deployment.agents.map((b) => ({
    value: b.agentId,
    label: b.enabled
      ? b.displayName
      : `${b.displayName} · disabled, enabled when saved as default`,
  }));
  const d = useDraft(
    {
      displayName: agent.displayName ?? "",
      defaultAgentId: agent.defaultAgentId ?? "",
      enabled: agent.enabled ?? true,
      tags: deployment.nodeConfig?.tags ?? [],
    },
    async (next) => {
      /* a new default lands with its enablement first; the node's other
         fields then save against an already valid default */
      if (next.defaultAgentId !== agent.defaultAgentId)
        await saveDefault(changeConfig, deployment, next.defaultAgentId);
      await changeConfig("saveNodeConfig", {
        document: {
          node_did: agent.nodeDid,
          display_name: next.displayName.trim(),
          default_agent_id: next.defaultAgentId,
          enabled: next.enabled,
          created_at: agent.createdAt,
          created_by: agent.createdBy,
          tags: next.tags.length ? next.tags : null,
        },
      });
    },
    {
      problems: (next) => ({
        displayName: next.displayName.trim() ? undefined : "Display name is required",
        defaultAgentId: next.defaultAgentId ? undefined : "Default agent is required",
      }),
    },
  );

  return (
    <div>
      <AgentCard deployment={deployment} />
      <Group title="Agent details">
        <TextRow
          id="agent-name"
          label="Display name"
          description="How this agent is named across the desktop."
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          error={d.problems.displayName}
        />
        <RefRow
          id="agent-default"
          label="Default agent"
          description="Used when a session does not choose one."
          value={d.draft.defaultAgentId}
          onChange={(v) => d.set("defaultAgentId", v)}
          error={d.problems.defaultAgentId}
          items={agents}
          createLabel="New agent…"
          openRoute={(agentId) => ({
            name: "agent",
            nodeDid: deployment.nodeDid,
            section: "agents",
            item: agentId,
          })}
        />
        <SwitchRow
          id="agent-enabled"
          label="Enabled"
          description="A disabled agent accepts no requests."
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
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
        draft={d}
        fields={{ displayName: "agent-name", defaultAgentId: "agent-default" }}
      />

      <Group title="Identity">
        <Row
          label="Node DID"
          description="The node's cryptographic identity. Permissions and audit are keyed by it."
        >
          <Fact mono>{agent.nodeDid}</Fact>
        </Row>
        <Row label="Install name">
          <Fact>{bootstrap?.initNodeName}</Fact>
        </Row>
        <Row label="Tool ceiling" description="The most any agent on this node may do.">
          <Fact>{bootstrap?.initToolCeiling ?? "not configured"}</Fact>
        </Row>
        <Row label="Tool root" description="The directory tools are confined to.">
          <Fact mono>{bootstrap?.initToolRoot ?? "not configured"}</Fact>
        </Row>
        <Row label="Peer" description="Where the node's runtime runs.">
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
          description="The last pass of the node's runtime over its configuration."
        >
          <Fact>
            {deployment.runtime?.reconcilePhase} ·{" "}
            {deployment.runtime?.lastReconcileResult}
          </Fact>
        </Row>
        <Row label="Executors" description="Agent executors in use over capacity.">
          <Fact>
            {deployment.runtime?.agentExecutorQueueDepth ?? 0} /{" "}
            {deployment.runtime?.agentExecutorCapacity ?? 0}
          </Fact>
        </Row>
      </Group>
      {isLocalNode(deployment, bootstrap?.initNodeDid) && <LocalServer />}
    </div>
  );
}
