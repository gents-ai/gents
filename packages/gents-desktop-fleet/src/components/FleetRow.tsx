import { useState, type KeyboardEvent, type MouseEvent } from "react";

import type {
  BootstrapSummary,
  DeploymentView,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import { isTerminalTurnState } from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import {
  ChatIcon,
  ConfigIcon,
  PencilIcon,
  ToolIconGlyph,
  TrashIcon,
} from "./FleetIcons.js";
import {
  deploymentStatus,
  formatRelativeTime,
  inferenceBackendTitle,
  needsInferenceSetup,
  toolCeilingIcons,
  type ToolIcon,
} from "../fleetMetrics.js";

export type FleetRowProps = {
  bootstrap: BootstrapSummary | null;
  deployment: DeploymentView;
  syncHealth?: SyncHealthView | null;
  onOpenChat: (nodeDid: string) => void;
  onOpenConfig: (nodeDid: string) => void;
  onRemoveRuntime?: (nodeDid: string) => Promise<unknown> | void;
  onRenameRuntime?: (nodeDid: string, label: string) => Promise<unknown> | void;
  onSetupInference?: (deployment: DeploymentView) => void;
};

export function FleetRow({
  bootstrap,
  deployment,
  syncHealth = null,
  onOpenChat,
  onOpenConfig,
  onRemoveRuntime,
  onRenameRuntime,
  onSetupInference,
}: FleetRowProps) {
  const [editingLabel, setEditingLabel] = useState<string | null>(null);
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const status = deploymentStatus(deployment, syncHealth);
  const localRuntime = deployment.local;
  const remoteRuntime = !deployment.operatorAccess;
  const chatReady = status.chatReady;

  function commitRename() {
    const label = (editingLabel ?? "").trim();
    setEditingLabel(null);
    if (label && label !== deployment.label && onRenameRuntime) {
      void Promise.resolve(onRenameRuntime(deployment.nodeDid, label)).catch(
        () => {},
      );
    }
  }
  const enabledTaskCount = deployment.tasks.filter(
    (task) => task.enabled !== false,
  ).length;
  const backendCount = deployment.inferenceBackends.filter(
    (backend) => backend.enabled !== false,
  ).length;
  const inferenceSetupNeeded = needsInferenceSetup(deployment);
  const openWorkCount = deployment.sessions.filter(
    (session) => session.turnState && !isTerminalTurnState(session.turnState),
  ).length;
  const defaultAgent = deployment.agents.find(
    (agent) => agent.agentDid === deployment.node.defaultAgentDid,
  );
  const toolIcons = toolCeilingIcons(
    deployment.tools,
    deployment.contexts.find(
      (context) => context.context_id === defaultAgent?.contextId,
    )?.tools_id,
    deployment.local ? bootstrap?.initToolCeiling : null,
  );
  const runtimeLastUpdate = deployment.runtime?.updatedAt ?? null;

  function openDetailsFromCard(
    event: MouseEvent<HTMLTableRowElement> | KeyboardEvent<HTMLTableRowElement>,
  ) {
    const target = event.target;
    if (
      target instanceof Element &&
      target.closest("button, input, select, textarea, a")
    ) {
      return;
    }
    onOpenChat(deployment.nodeDid);
  }

  return (
    <tr
      aria-label={`Open ${deployment.label} details`}
      className="fleet-deployment-row"
      data-testid={`fleet-row-${deployment.nodeDid}`}
      onClick={openDetailsFromCard}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          openDetailsFromCard(event);
        }
      }}
      role="button"
      tabIndex={0}
    >
      <td>
        <div className="fleet-agent-cell">
          <span
            aria-label={status.label}
            className={`fleet-status ${status.tone}`}
            title={status.title}
          >
            <span
              aria-hidden="true"
              className={`fleet-status-dot ${status.tone}`}
            />
            <span
              className="fleet-status-label"
              data-testid={`fleet-status-${deployment.nodeDid}`}
            >
              {status.label}
            </span>
          </span>
          <div className="fleet-agent-copy">
            {editingLabel != null ? (
              <input
                autoFocus
                className="fleet-rename-input"
                data-testid={`fleet-rename-input-${deployment.nodeDid}`}
                onBlur={commitRename}
                onChange={(event) => setEditingLabel(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    commitRename();
                  } else if (event.key === "Escape") {
                    setEditingLabel(null);
                  }
                }}
                value={editingLabel}
              />
            ) : (
              <button
                className="fleet-agent-name"
                data-testid={`fleet-detail-name-${deployment.nodeDid}`}
                onClick={() => onOpenChat(deployment.nodeDid)}
                title={`Open ${deployment.label} details`}
                type="button"
              >
                {deployment.label}
              </button>
            )}
            <span
              className="muted fleet-agent-summary"
              data-testid={`fleet-summary-${deployment.nodeDid}`}
            >
              {deployment.agents.length} agents · {deployment.sessions.length}{" "}
              sessions · {deployment.tasks.length} tasks
            </span>
            {status.lastError ? (
              <span
                className="fleet-agent-error"
                data-testid={`fleet-error-${deployment.nodeDid}`}
              >
                {status.lastError}
              </span>
            ) : null}
          </div>
          {onRenameRuntime && editingLabel == null ? (
            <button
              aria-label={`Rename ${deployment.label}`}
              className="ghost-button fleet-row-icon fleet-rename"
              data-testid={`fleet-rename-${deployment.nodeDid}`}
              onClick={() => setEditingLabel(deployment.label)}
              title="Rename deployment (saved label)"
              type="button"
            >
              <PencilIcon />
            </button>
          ) : null}
        </div>
      </td>
      <td>
        <Metric value={deployment.agents.length} label="total" />
      </td>
      <td>
        <Metric value={enabledTaskCount} label="enabled" />
      </td>
      <td>
        <div className="fleet-inference-cell">
          <Metric
            label={
              remoteRuntime
                ? "host-managed"
                : backendCount === 1
                  ? "backend"
                  : "backends"
            }
            title={inferenceBackendTitle(deployment)}
            value={remoteRuntime ? "—" : backendCount}
          />
          {inferenceSetupNeeded ? (
            onSetupInference ? (
              <button
                className="ghost-button fleet-inference-needed"
                data-testid={`fleet-inference-setup-${deployment.nodeDid}`}
                onClick={() => onSetupInference(deployment)}
                title={`Configure inference for ${deployment.label}`}
                type="button"
              >
                Setup needed
              </button>
            ) : (
              <span className="fleet-inference-needed">Setup needed</span>
            )
          ) : null}
        </div>
      </td>
      <td>
        <ToolIconStrip icons={toolIcons} />
      </td>
      <td>
        <Metric title="Processing sessions" value={openWorkCount} />
      </td>
      <td title="Last runtime state change reported by this node (nodes write this on change, not on a timer — an idle node ages here without being dead)">
        {formatRelativeTime(runtimeLastUpdate)}
      </td>
      <td className="fleet-actions-cell">
        <div className="fleet-row-actions">
          <button
            aria-label={`Open ${deployment.label} chat`}
            className="primary-button fleet-table-action fleet-open-chat-action"
            data-testid={`fleet-chat-${deployment.nodeDid}`}
            disabled={!chatReady}
            onClick={() => onOpenChat(deployment.nodeDid)}
            title={
              chatReady
                ? "Open chat"
                : "Chat becomes available after runtime access and agent readiness are confirmed"
            }
            type="button"
          >
            <ChatIcon />
          </button>
          <button
            aria-label={`Configure ${deployment.label}`}
            className="ghost-button fleet-table-action"
            data-testid={`fleet-config-${deployment.nodeDid}`}
            onClick={() => onOpenConfig(deployment.nodeDid)}
            title="Configure node"
            type="button"
          >
            <ConfigIcon />
          </button>
          {onRemoveRuntime && !localRuntime ? (
            <button
              aria-label={`Remove ${deployment.label}`}
              className="ghost-button fleet-table-action danger-button"
              data-testid={`fleet-remove-${deployment.nodeDid}`}
              onClick={() => setConfirmingRemove(true)}
              title="Remove saved deployment"
              type="button"
            >
              <TrashIcon />
            </button>
          ) : null}
          <ConfirmDialog
            open={confirmingRemove}
            title="Remove deployment"
            message={`Remove "${deployment.label}" from this desktop's saved deployments? The remote node itself is not touched.`}
            confirmLabel="Remove"
            danger
            onConfirm={() => {
              setConfirmingRemove(false);
              void Promise.resolve(onRemoveRuntime?.(deployment.nodeDid)).catch(
                () => {},
              );
            }}
            onCancel={() => setConfirmingRemove(false)}
          />
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
  value: number | string;
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
