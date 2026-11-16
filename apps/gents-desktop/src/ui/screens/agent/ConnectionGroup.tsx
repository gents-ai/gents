import { cn } from "@gents/ui/lib/utils";
import type { NodeView } from "../../../hooks/fleetStore";
import { leaseLeft } from "../../../hooks/nodeAccess";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { joinedPeerId } from "@/lib/nodes";
import { StateDot, useNodeConnection } from "../NodeFacts";
import { CopyButton } from "../sharing/parts";
import { Fact, Group, Row } from "./rows";

export const CONNECTION_GROUP = "connection";

export function ConnectionGroup({ deployment }: { deployment: NodeView }) {
  const homeDid = useHomeDid();
  const connection = useNodeConnection(deployment);
  const local = isLocalNode(deployment, homeDid);
  const peerId = joinedPeerId(deployment, homeDid);
  const lease = local ? null : leaseLeft(deployment.leaseExpiresAt);
  return (
    <Group id={CONNECTION_GROUP} title="Connection">
      <Row label="Status" description={connection.detail}>
        <span
          data-testid="node-connection-status"
          className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground"
        >
          <StateDot tone={connection.tone} />
          {connection.label}
        </span>
      </Row>
      <Row label="Address" description="Where this app reaches the node.">
        <Fact mono>{deployment.addr || null}</Fact>
      </Row>
      <Row label="Node">
        <Fact>{local ? "Local node" : "Remote node"}</Fact>
      </Row>
      {peerId && (
        <Row label="Peer ID" description="The node's peer ID on the network.">
          <span className="flex min-w-0 items-center gap-1">
            <Fact mono>{peerId}</Fact>
            <CopyButton text={peerId} label="Copy peer ID" />
          </span>
        </Row>
      )}
      {lease && (
        <Row label="Access" description="How long this app may use the node.">
          <span
            data-testid="node-connection-lease"
            title={
              deployment.leaseExpiresAt
                ? new Date(deployment.leaseExpiresAt).toLocaleString()
                : undefined
            }
            className={cn(
              "text-sm",
              lease.soon ? "text-warning" : "text-muted-foreground",
            )}
          >
            {lease.text}
          </span>
        </Row>
      )}
      <Row
        label="Operator access"
        description="Whether this app can change the node's configuration. The node still decides each change."
      >
        <Fact>{deployment.operatorAccess ? "Yes" : "No"}</Fact>
      </Row>
      {deployment.lastError && (
        <Row label="Last error">
          <Fact>{deployment.lastError}</Fact>
        </Row>
      )}
    </Group>
  );
}
