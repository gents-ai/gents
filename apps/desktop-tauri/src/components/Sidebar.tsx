import type { FormEvent } from "react";

import type {
  ConversationSummary,
  DeploymentView,
  InitSummary,
  P2PHealth,
} from "../lib/types";
import { displayAgentIdentity, displayConversationTitle } from "../lib/types";

type SidebarProps = {
  running: boolean;
  starting: boolean;
  stopping: boolean;
  runtimeHealth: P2PHealth | null;
  deployments: DeploymentView[];
  conversations: ConversationSummary[];
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
  initializing: boolean;
  initSummary: InitSummary | null;
  onLabelChange: (value: string) => void;
  onDangerouslyOverwriteChange: (value: boolean) => void;
  onResetChange: (value: boolean) => void;
  onRefresh: () => void;
  onSelectDeployment: (agentDid: string) => void;
  onSelectSession: (sessionId: string) => void;
  onShutdown: () => void;
  onStart: () => void;
  onInit: (event: FormEvent) => void;
};

function conversationStatusClass(conversation: ConversationSummary) {
  const state = (conversation.turnState ?? conversation.status ?? "").toLowerCase();

  switch (state) {
    case "completed":
      return "conversation-status-dot conversation-status-dot-success";
    case "failed":
    case "error":
    case "cancelled":
      return "conversation-status-dot conversation-status-dot-error";
    case "streaming":
    case "waitingforclaim":
    case "processing":
    case "active":
      return "conversation-status-dot conversation-status-dot-running";
    default:
      return "conversation-status-dot conversation-status-dot-idle";
  }
}

export function Sidebar({
  running,
  starting,
  stopping,
  runtimeHealth,
  deployments,
  conversations,
  selectedAgentDid,
  selectedSessionId,
  label,
  dangerouslyOverwrite,
  reset,
  initializing,
  initSummary,
  onLabelChange,
  onDangerouslyOverwriteChange,
  onResetChange,
  onRefresh,
  onSelectDeployment,
  onSelectSession,
  onShutdown,
  onStart,
  onInit,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <section className="sidebar-section">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Deployments</p>
            <h2>Saved Peers</h2>
          </div>
        </div>
        {!deployments.length ? (
          <p className="muted">
            No saved deployments yet. Run desktop init to create the local
            runtime and save the first peer.
          </p>
        ) : (
          <div className="list">
            {deployments.map((deployment) => (
              <button
                className={
                  deployment.agentDid === selectedAgentDid
                    ? "list-item selected"
                    : "list-item"
                }
                data-testid={`deployment-${deployment.peerId}`}
                key={deployment.peerId}
                onClick={() => onSelectDeployment(deployment.agentDid)}
              >
                <span className="list-item-title">{deployment.label}</span>
                <span className="list-item-meta">
                  {deployment.dialSucceeded ? "connected" : "saved"}
                </span>
                {displayAgentIdentity(deployment.agentDid) ? (
                  <span className="list-item-subtle">
                    {displayAgentIdentity(deployment.agentDid)}
                  </span>
                ) : null}
              </button>
            ))}
          </div>
        )}
      </section>

      <section className="sidebar-section">
        <div className="panel-header">
          <div>
            <p className="eyebrow">Conversations</p>
            <h2>{deployments.find((item) => item.agentDid === selectedAgentDid)?.label ?? "Chat"}</h2>
          </div>
        </div>
        {!selectedAgentDid ? (
          <p className="muted">Select a deployment to see conversations.</p>
        ) : !conversations.length ? (
          <p className="muted">
            No conversations yet. Sending the first message will create one automatically.
          </p>
        ) : (
          <div className="list">
            {conversations.map((conversation) => (
              <button
                className={
                  conversation.sessionId === selectedSessionId
                    ? "list-item selected"
                    : "list-item"
                }
                data-testid={`conversation-${conversation.sessionId}`}
                key={conversation.sessionId}
                onClick={() => onSelectSession(conversation.sessionId)}
              >
                <span className="conversation-list-row">
                  <span
                    aria-hidden="true"
                    className={conversationStatusClass(conversation)}
                  />
                  <span
                    className={
                      conversation.title ? "list-item-title conversation-list-title" : "list-item-title conversation-list-title untitled-title"
                    }
                  >
                    {displayConversationTitle(conversation.title)}
                  </span>
                </span>
              </button>
            ))}
          </div>
        )}
      </section>

      <details className="sidebar-utility">
        <summary>Local Runtime Setup</summary>
        <div className="utility-actions">
          <button className="ghost-button" onClick={onRefresh} type="button">
            Refresh
          </button>
          {!running ? (
            <button
              className="primary-button"
              disabled={starting}
              onClick={onStart}
              type="button"
            >
              {starting ? "Starting…" : "Start Core"}
            </button>
          ) : (
            <button
              className="ghost-button"
              disabled={stopping}
              onClick={onShutdown}
              type="button"
            >
              {stopping ? "Stopping…" : "Shutdown Core"}
            </button>
          )}
        </div>
        <div className="utility-status">
          <span
            className={
              runtimeHealth?.status === "healthy" ? "chip chip-green" : "chip"
            }
          >
            {running ? runtimeHealth?.status ?? "running" : "stopped"}
          </span>
        </div>
        <form className="stack compact-stack" onSubmit={onInit}>
          <label className="field">
            <span>Saved deployment label</span>
            <input
              value={label}
              onChange={(event) => onLabelChange(event.currentTarget.value)}
              placeholder="Local Agent"
            />
          </label>
          <label className="checkbox">
            <input
              checked={dangerouslyOverwrite}
              onChange={(event) =>
                onDangerouslyOverwriteChange(event.currentTarget.checked)
              }
              type="checkbox"
            />
            <span>Dangerously overwrite desktop home</span>
          </label>
          <label className="checkbox">
            <input
              checked={reset}
              onChange={(event) => onResetChange(event.currentTarget.checked)}
              type="checkbox"
            />
            <span>Reset desktop runtime state</span>
          </label>
          <button className="primary-button" disabled={initializing}>
            {initializing ? "Initializing…" : "Run desktop init"}
          </button>
        </form>
        {initSummary ? (
          <div className="callout success">
            <h3>Init complete</h3>
            <p>{initSummary.label}</p>
            <p className="mono">{initSummary.agentDid}</p>
          </div>
        ) : null}
        <p className="muted small">
          Desktop core is currently {running ? "running" : "stopped"}.
        </p>
      </details>
    </aside>
  );
}
