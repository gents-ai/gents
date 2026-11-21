import { ClientAccess } from "../sharing/ClientAccess";
import type { NodeView } from "../../../hooks/fleetStore";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { StateDot, useNodeConnection } from "../NodeFacts";
import { Fact, Group, Row } from "./rows";

export const CONNECTION_GROUP = "connection";

export function ConnectionGroup({ deployment }: { deployment: NodeView }) {
  const homeDid = useHomeDid();
  const connection = useNodeConnection(deployment);
  const local = isLocalNode(deployment, homeDid);
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
        <Fact mono>{deployment.graphql || null}</Fact>
      </Row>
      <Row label="Node">
        <Fact>{local ? "Local node" : "Remote node"}</Fact>
      </Row>
      <Row
        label="Operator access"
        description="Whether this app can change the node's configuration. The node still decides each change."
      >
        <Fact>{deployment.operatorAccess ? "Yes" : "No"}</Fact>
      </Row>
      {deployment.operatorAccess && (
        <ClientAccess key={deployment.nodeDid} nodeDid={deployment.nodeDid} />
      )}
    </Group>
  );
}
