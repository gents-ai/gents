import { useEffect, useState } from "react";
import type { NodeView } from "../../../hooks/fleetStore";
import { isLocalNode } from "@/lib/firstRun";
import { DraftActions, NumberRow, RefRow, TagsRow, TextRow } from "./editors";
import { useDraft } from "./draft";
import { Fact, focusField, Group, PathFact, Row, Unfold } from "./rows";
import { LocalServer } from "./LocalServer";
import { AgentCard } from "./AgentCard";
import { saveDefault } from "./BehaviorsPanel";
import { packOrigin } from "./ListDetail";
import { useApp } from "@/app/AppContext";
import { settleAvatar } from "@/lib/avatarRoll";
import { useBootstrap, useNodeConfigReadOnly, useNodeReach } from "@/hooks/useClient";
import { href } from "@/lib/router";

export function AgentPanel({ deployment }: { deployment: NodeView }) {
  const bootstrap = useBootstrap();
  const reach = useNodeReach(deployment);
  const { changeConfig } = useApp().actions;
  const agent = deployment.node;
  /* settings the app hasn't read are unknown, not empty: no form is drawn
     from them, so nothing reads as missing or could be saved over them */
  const readOnly = useNodeConfigReadOnly(deployment);
  const agents = deployment.agents.map((b) => ({
    value: b.agentDid,
    pack: packOrigin(b.tags),
    label: b.enabled
      ? b.displayName
      : `${b.displayName} · disabled, enabled when saved as default`,
  }));
  /* the name and tags show in the header; their fields open under it */
  const [naming, setNaming] = useState(false);
  const d = useDraft(
    {
      displayName: agent.displayName ?? "",
      defaultAgentDid: agent.defaultAgentDid ?? "",
      enabled: agent.enabled ?? true,
      maxRequestHop: deployment.nodeConfig?.max_request_hop?.toString() ?? "",
      tags: deployment.nodeConfig?.tags ?? [],
      avatar: deployment.nodeConfig?.avatar ?? "",
    },
    async (next) => {
      if (next.defaultAgentDid !== agent.defaultAgentDid)
        await saveDefault(changeConfig, deployment, next.defaultAgentDid);
      await changeConfig("saveNodeConfig", {
        document: {
          node_did: agent.nodeDid,
          display_name: next.displayName.trim(),
          avatar: next.avatar || null,
          default_agent_did: next.defaultAgentDid,
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
        defaultAgentDid: next.defaultAgentDid ? undefined : "Default agent is required",
        maxRequestHop:
          next.maxRequestHop.trim() === "" ||
          (/^\d+$/.test(next.maxRequestHop) && Number(next.maxRequestHop) <= 2147483647)
            ? undefined
            : "Enter a whole number from 0 to 2147483647, or leave blank.",
      }),
    },
  );

  /* a rolled portrait shows across the desktop while it is unsaved, and
     the saved one again once it is saved or set aside */
  const savedAvatar = deployment.nodeConfig?.avatar ?? "";
  const nodeDid = deployment.nodeDid;
  useEffect(() => {
    if (d.draft.avatar === savedAvatar) settleAvatar(nodeDid);
  }, [d.draft.avatar, savedAvatar, nodeDid]);
  useEffect(() => () => settleAvatar(nodeDid), [nodeDid]);

  return (
    <div>
      <AgentCard
        deployment={deployment}
        edit={
          readOnly
            ? undefined
            : {
                open: naming,
                onToggle: () => setNaming((open) => !open),
                onTags: () => {
                  setNaming(true);
                  focusField("agent-tags");
                },
                controls: "node-naming",
                enabled: d.draft.enabled,
                onEnabled: (v) => d.set("enabled", v),
                tags: d.draft.tags,
              }
        }
        avatar={d.draft.avatar || null}
        onAvatar={(v) => d.set("avatar", v)}
      />
      {readOnly && <SharedDetails deployment={deployment} />}
      {!readOnly && (
        <>
          <Unfold open={Boolean(naming || d.problems.displayName)} id="node-naming">
            <Group title="Name and tags">
              <TextRow
                id="agent-name"
                label="Display name"
                description="How this node is named across the desktop."
                value={d.draft.displayName}
                onChange={(v) => d.set("displayName", v)}
                error={d.problems.displayName}
              />
              <TagsRow
                id="agent-tags"
                label="Tags"
                description="Optional discovery labels."
                value={d.draft.tags}
                onChange={(v) => d.set("tags", v)}
              />
            </Group>
          </Unfold>
          <Group title="Node details">
            <RefRow
              id="agent-default"
              label="Default agent"
              description="Used when a session does not choose one."
              value={d.draft.defaultAgentDid}
              onChange={(v) => d.set("defaultAgentDid", v)}
              error={d.problems.defaultAgentDid}
              items={agents}
              createLabel="New agent…"
              openRoute={(agentDid) => ({
                name: "agent",
                nodeDid: deployment.nodeDid,
                section: "agents",
                item: agentDid,
              })}
            />
            <NumberRow
              id="agent-max-request-hop"
              label="Maximum request hop"
              description="Limits how far requests can be chained. Leave blank for no limit; 0 permits only root requests."
              value={d.draft.maxRequestHop}
              onChange={(v) => d.set("maxRequestHop", v)}
              error={d.problems.maxRequestHop}
            />
          </Group>
          <DraftActions
            draft={d}
            fields={{
              displayName: "agent-name",
              defaultAgentDid: "agent-default",
              maxRequestHop: "agent-max-request-hop",
            }}
          />
        </>
      )}
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

/* what a node shares of its own settings, shown as facts where the app can't
   change them; a node on an older build, or whose settings this window may
   not read, shares less, and says so rather than looking unset */
function SharedDetails({ deployment }: { deployment: NodeView }) {
  const node = deployment.node;
  const config = deployment.nodeConfig;
  const unshared = "Not shared by this node";
  const defaultId = node.defaultAgentDid;
  const defaultName =
    deployment.agents.find((a) => a.agentDid === defaultId)?.displayName ?? defaultId;
  return (
    <Group title="Node details">
      <Row label="Default agent" description="Used when a session does not choose one.">
        <Fact title={defaultName ?? unshared}>
          {defaultId ? (
            <a
              href={href({
                name: "agent",
                nodeDid: deployment.nodeDid,
                section: "agents",
                item: defaultId,
              })}
              className="underline-offset-2 hover:text-foreground hover:underline"
            >
              {defaultName}
            </a>
          ) : (
            unshared
          )}
        </Fact>
      </Row>
      <Row
        label="Maximum request hop"
        description="Limits how far requests can be chained; 0 permits only root requests."
      >
        <Fact>
          {config ? (config.max_request_hop?.toString() ?? "No limit") : unshared}
        </Fact>
      </Row>
    </Group>
  );
}
