import { isTerminalTurnState } from "../../lib/chat-shell";
import type { BootstrapSummary, DeploymentView } from "../../lib/types";
import {
  displayAgentIdentity,
  displayBehaviorLabel,
  displayGraphqlEndpoint,
} from "../../lib/types";
import { ChatIcon, ConfigIcon, ToolIconGlyph } from "./FleetIcons";
import {
  deploymentStatus,
  formatRelativeTime,
  inferenceBackendTitle,
  isLocalRuntimeSource,
  toolCeilingIcons,
  type ToolIcon,
} from "./fleetMetrics";

export type FleetRowProps = {
  bootstrap: BootstrapSummary | null;
  deployment: DeploymentView;
  onOpenChat: (agentDid: string) => void;
  onOpenConfig: (agentDid: string) => void;
};

export function FleetRow({
  bootstrap,
  deployment,
  onOpenChat,
  onOpenConfig,
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
    isLocalRuntimeSource(deployment.source) ? bootstrap?.initToolCeiling : null,
  );
  const agentIdentity = displayAgentIdentity(deployment.agentDid);
  const graphqlEndpoint = displayGraphqlEndpoint(deployment.graphql);
  const defaultBehaviorLabel = displayBehaviorLabel(
    deployment.defaultBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId,
  );
  // Per-deployment reconciler heartbeat — NOT the desktop client's global
  // P2P probe, which is identical for every row and lies about dead peers.
  const runtimeLastUpdate = deployment.runtime?.updatedAt ?? null;

  return (
    <tr data-testid={`fleet-row-${deployment.peerId}`}>
      <td>
        <div className="fleet-agent-cell">
          <span className={`fleet-status-dot ${status.tone}`} title={status.title} />
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
      <td title="Last runtime state change reported by this agent (agents write this on change, not on a timer — an idle agent ages here without being dead)">
        {formatRelativeTime(runtimeLastUpdate)}
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
