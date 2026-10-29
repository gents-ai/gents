import type { NodeView } from "../../../hooks/fleetStore";
import { isLocalNode } from "@/lib/firstRun";
import {
  DraftActions,
  NumberRow,
  RefRow,
  SwitchRow,
  TagsRow,
  TextRow,
} from "./editors";
import { useDraft } from "./draft";
import { Fact, Group, PathFact, Row } from "./rows";
import { LocalServer } from "./LocalServer";
import { AgentCard } from "./AgentCard";
import { saveDefault } from "./BehaviorsPanel";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useNodeReach } from "@/hooks/useClient";

export function AgentPanel({ deployment }: { deployment: NodeView }) {
  const bootstrap = useBootstrap();
  const reach = useNodeReach(deployment);
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
      maxRequestHop: deployment.nodeConfig?.max_request_hop?.toString() ?? "",
      tags: deployment.nodeConfig?.tags ?? [],
    },
    async (next) => {
      if (next.defaultAgentId !== agent.defaultAgentId)
        await saveDefault(changeConfig, deployment, next.defaultAgentId);
      await changeConfig("saveNodeConfig", {
        document: {
          node_did: agent.nodeDid,
          display_name: next.displayName.trim(),
          default_agent_id: next.defaultAgentId,
          enabled: next.enabled,
          max_request_hop:
            next.maxRequestHop.trim() === "" ? null : Number(next.maxRequestHop),
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
        maxRequestHop:
          next.maxRequestHop.trim() === "" ||
          (/^\d+$/.test(next.maxRequestHop) && Number(next.maxRequestHop) <= 2147483647)
            ? undefined
            : "Enter a whole number from 0 to 2147483647, or leave blank.",
      }),
    },
  );

  return (
    <div>
      <AgentCard deployment={deployment} />
      <Group title="Node details">
        <TextRow
          id="agent-name"
          label="Display name"
          description="How this node is named across the desktop."
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
          description="A disabled node accepts no requests."
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
        />
        <NumberRow
          id="agent-max-request-hop"
          label="Maximum request hop"
          description="Limits how far requests can be chained. Leave blank for no limit; 0 permits only root requests."
          value={d.draft.maxRequestHop}
          onChange={(v) => d.set("maxRequestHop", v)}
          error={d.problems.maxRequestHop}
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
        fields={{
          displayName: "agent-name",
          defaultAgentId: "agent-default",
          maxRequestHop: "agent-max-request-hop",
        }}
      />

      <Group title="Identity">
        <Row
          label="Node DID"
          description="The node's cryptographic identity. Permissions and audit are keyed by it."
        >
          <Fact mono>{agent.nodeDid}</Fact>
        </Row>
        {reach.local && (
          <>
            <Row label="Install name">
              <Fact>{bootstrap?.initNodeName}</Fact>
            </Row>
            <Row
              label="Tool ceiling"
              description="The most any agent on this node may do."
            >
              <Fact>{reach.ceiling ?? "not configured"}</Fact>
            </Row>
          </>
        )}
        <Row label="Tool root" description="The directory tools are confined to.">
          <PathFact path={reach.root} local={reach.local} />
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
