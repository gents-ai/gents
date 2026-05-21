import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  AgentConfigSaveRequest,
  AgentPrincipalView,
  BehaviorView,
  BootstrapSummary,
  DeploymentView,
} from "../../lib/types";
import { PencilIcon } from "./ConfigChrome";
import { ignoreHandledActionError } from "./formUtils";

export type AgentConfigPanelProps = {
  bootstrap: BootstrapSummary | null;
  deployment: DeploymentView;
  saving: boolean;
  savedStatus: string | null;
  onSavedStatusChange: (value: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
};

export function AgentConfigPanel({
  bootstrap,
  deployment,
  saving,
  savedStatus,
  onSavedStatusChange,
  onSaveAgentConfig,
}: AgentConfigPanelProps) {
  return (
    <section className="config-single-layout">
      <AgentConfigEditor
        agent={deployment.agentPrincipal}
        behaviors={deployment.behaviors}
        bootstrap={bootstrap}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(agentDid) => onSavedStatusChange(`agent:${agentDid}`)}
        onSaveAgentConfig={onSaveAgentConfig}
      />
    </section>
  );
}

export type AgentConfigEditorProps = {
  agent: AgentPrincipalView;
  behaviors: BehaviorView[];
  bootstrap: BootstrapSummary | null;
  saving: boolean;
  savedStatus: string | null;
  onSaved: (agentDid: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
};

export function AgentConfigEditor({
  agent,
  behaviors,
  bootstrap,
  saving,
  savedStatus,
  onSaved,
  onSaveAgentConfig,
}: AgentConfigEditorProps) {
  const [displayName, setDisplayName] = useState("");
  const [editingDisplayName, setEditingDisplayName] = useState(false);

  useEffect(() => {
    setDisplayName(agent.displayName ?? "");
    setEditingDisplayName(false);
  }, [agent]);

  const defaultBehaviorId =
    agent.defaultBehaviorId ??
    behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
    behaviors[0]?.behaviorId ??
    "";

  async function submitAgent(event: FormEvent) {
    event.preventDefault();
    try {
      await onSaveAgentConfig({
        agentDid: agent.agentDid,
        displayName,
        defaultBehaviorId,
        enabled: agent.enabled ?? true,
      });
      setEditingDisplayName(false);
      onSaved(agent.agentDid);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor agent-config-editor" onSubmit={submitAgent}>
      <div className="panel-header agent-config-header">
        <div>
          <p className="eyebrow">Agent</p>
          <div className="agent-name-row">
            {editingDisplayName ? (
              <input
                className="agent-name-input"
                data-testid="agent-display-name"
                onChange={(event) => setDisplayName(event.currentTarget.value)}
                value={displayName}
              />
            ) : (
              <h3>{displayName || "Agent"}</h3>
            )}
            {!editingDisplayName ? (
              <button
                aria-label="Edit agent display name"
                className="ghost-button config-icon-button"
                data-testid="agent-edit-display-name"
                onClick={() => setEditingDisplayName(true)}
                title="Edit display name"
                type="button"
              >
                <PencilIcon />
              </button>
            ) : null}
          </div>
        </div>
        {savedStatus === `agent:${agent.agentDid}` ? (
          <span className="chip chip-green">Saved</span>
        ) : null}
      </div>

      <div className="facts agent-install-facts">
        <div>
          <dt>Agent DID</dt>
          <dd className="mono" title={agent.agentDid}>
            {agent.agentDid}
          </dd>
        </div>
        <div>
          <dt>Install name</dt>
          <dd>{bootstrap?.initAgentName ?? "unknown"}</dd>
        </div>
        <div>
          <dt>Tool ceiling</dt>
          <dd>{bootstrap?.initToolCeiling ?? "unknown"}</dd>
        </div>
        <div>
          <dt>Tool root</dt>
          <dd className="mono" title={bootstrap?.initToolRoot ?? "not configured"}>
            {bootstrap?.initToolRoot ?? "not configured"}
          </dd>
        </div>
      </div>

      {editingDisplayName ? (
        <div className="config-actions agent-config-actions">
          <button
            className="ghost-button"
            onClick={() => {
              setDisplayName(agent.displayName ?? "");
              setEditingDisplayName(false);
            }}
            type="button"
          >
            Cancel
          </button>
          <button
            className="primary-button"
            data-testid="agent-save"
            disabled={saving || !displayName.trim() || !defaultBehaviorId.trim()}
            type="submit"
          >
            {saving ? "Saving..." : "Save Agent"}
          </button>
        </div>
      ) : null}
    </form>
  );
}
