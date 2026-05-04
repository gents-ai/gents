import { isTerminalTurnState } from "../../lib/chat-shell";
import type { BootstrapSummary, DeploymentView, P2PHealth } from "../../lib/types";
import {
  displayAgentIdentity,
  displayBehaviorLabel,
  displayGraphqlEndpoint,
} from "../../lib/types";
import { ChatIcon, ConfigIcon, RepairIcon, ToolIconGlyph } from "./FleetIcons";
import { deploymentStatus, formatRelativeTime, inferenceBackendTitle, toolCeilingIcons, type ToolIcon } from "./fleetMetrics";

export type FleetRowProps = {
  bootstrap: BootstrapSummary | null;
  deployment: DeploymentView;
  p2pHealth: P2PHealth | null;
  repairingP2P: boolean;
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
  onRepairP2P: () => Promise<unknown>;
};

export function FleetRow({
  bootstrap,
  deployment,
  p2pHealth,
  repairingP2P,
  onOpenChat,
  onOpenConfig,
  onRepairP2P,
}: FleetRowProps) {
  const status = deploymentStatus(deployment);
  const enabledTaskCount = deployment.tasks.filter(
    (task) => task.enabled !== false,
  ).length;
  const backendCount = deployment.inferenceBackends.filter(
    (backend) => backend.enabled !== false,
  ).length;
  const openWorkCount = deployment.conversations.filter(
    (conversation) =>
      conversation.turnState && !isTerminalTurnState(conversation.turnState),
  ).length;
  const defaultBehavior = deployment.behaviors.find(
    (behavior) =>
      behavior.behaviorId ===
      (deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId),
  );
  const toolIcons = toolCeilingIcons(
    deployment.toolSelections,
    defaultBehavior?.toolSelectionId,
    bootstrap?.initToolCeiling,
  );
  const agentIdentity = displayAgentIdentity(deployment.agentDid);
  const graphqlEndpoint = displayGraphqlEndpoint(deployment.graphql);
  const defaultBehaviorLabel = displayBehaviorLabel(
    deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId,
  );
  const p2pLastUpdate = p2pHealth?.lastOkAt ?? p2pHealth?.lastFailureAt ?? null;
  const canRepairP2P = !deployment.dialSucceeded || Boolean(deployment.lastError);

  return (
    <tr data-testid={`fleet-row-${deployment.peerId}`}>
      <td>
        <div className="fleet-agent-cell">
          <span
            className={`fleet-status-dot ${status.tone}`}
            title={status.title}
          />
          <div className="fleet-agent-copy">
            <button
              className="fleet-agent-name"
              data-testid={`fleet-chat-name-${deployment.peerId}`}
              onClick={() => onOpenChat(deployment.agentDid)}
              title={`Open ${deployment.label} chat`}
              type="button"
            >
              {deployment.agentPrincipal.displayName ?? deployment.label}
            </button>
            <span className="muted mono">
              {[
                agentIdentity,
                defaultBehaviorLabel ? `default: ${defaultBehaviorLabel}` : null,
              ]
                .filter(Boolean)
                .join(" | ")}
            </span>
            {graphqlEndpoint ? (
              <span
                className="fleet-agent-endpoint mono"
                title={`GraphQL endpoint: ${deployment.graphql ?? ""}`}
              >
                GraphQL {graphqlEndpoint}
              </span>
            ) : null}
          </div>
        </div>
      </td>
      <td>
        <Metric value={deployment.behaviors.length} label="total" />
      </td>
      <td>
        <Metric value={enabledTaskCount} label="enabled" />
      </td>
      <td>
        <Metric
          label={backendCount === 1 ? "backend" : "backends"}
          title={inferenceBackendTitle(deployment)}
          value={backendCount}
        />
      </td>
      <td>
        <ToolIconStrip icons={toolIcons} />
      </td>
      <td>
        <Metric title="Processing conversations" value={openWorkCount} />
      </td>
      <td title="Last desktop P2P health probe">
        {formatRelativeTime(p2pLastUpdate)}
      </td>
      <td className="fleet-actions-cell">
        <div className="fleet-row-actions">
          <button
            aria-label={`Open ${deployment.label} chat`}
            className="primary-button fleet-table-action"
            data-testid={`fleet-chat-${deployment.peerId}`}
            onClick={() => onOpenChat(deployment.agentDid)}
            title="Open chat"
            type="button"
          >
            <ChatIcon />
          </button>
          <button
            aria-label={`Configure ${deployment.label}`}
            className="ghost-button fleet-table-action"
            data-testid={`fleet-config-${deployment.peerId}`}
            onClick={() => onOpenConfig(deployment.agentDid)}
            title="Configure agent"
            type="button"
          >
            <ConfigIcon />
          </button>
          <button
            aria-label={
              canRepairP2P
                ? `Repair ${deployment.label} P2P`
                : `${deployment.label} P2P healthy`
            }
            className="ghost-button fleet-table-action"
            data-testid={`fleet-repair-${deployment.peerId}`}
            disabled={!canRepairP2P || repairingP2P}
            onClick={() => void onRepairP2P()}
            title={canRepairP2P ? "Repair P2P" : "P2P healthy"}
            type="button"
          >
            <RepairIcon />
          </button>
        </div>
      </td>
    </tr>
  );
}

function Metric({
  label,
  title,
  value,
}: {
  label?: string;
  title?: string;
  value: number;
}) {
  return (
    <span className="fleet-metric" title={title}>
      {value}
      {label ? <span>{label}</span> : null}
    </span>
  );
}

function ToolIconStrip({ icons }: { icons: ToolIcon[] }) {
  if (!icons.length) {
    return <span className="muted">none</span>;
  }

  return (
    <div className="fleet-tool-icons">
      {icons.map((icon) => (
        <span
          className={`fleet-tool-icon ${icon.tone}`}
          key={`${icon.kind}-${icon.title}`}
          title={icon.title}
        >
          <ToolIconGlyph kind={icon.kind} />
        </span>
      ))}
    </div>
  );
}
