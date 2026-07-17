import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  AgentConfigSaveRequest,
  AgentPrincipalView,
  BehaviorView,
  BootstrapSummary,
  DeploymentView,
} from "../../lib/types";
import { EditorStatusChip, FieldHint, PencilIcon } from "./ConfigChrome";

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
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    setDisplayName(agent.displayName ?? "");
    setEditingDisplayName(false);
    setSaveError(null);
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
  }, [agent.agentDid]);

  const dirty = editingDisplayName && displayName !== (agent.displayName ?? "");

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
        // Only an in-progress rename may override the live document value —
        // a save without one must not resurrect a stale hydration.
        displayName: editingDisplayName ? displayName : (agent.displayName ?? ""),
        defaultBehaviorId,
        enabled: agent.enabled ?? true,
      });
      setEditingDisplayName(false);
      onSaved(agent.agentDid);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
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
              // Live document value: remote renames stay visible while no
              // edit is in progress.
              <h3>{agent.displayName || "Agent"}</h3>
            )}
            {!editingDisplayName ? (
              <button
                aria-label="Edit agent display name"
                className="ghost-button config-icon-button"
                data-testid="agent-edit-display-name"
                onClick={() => {
                  setDisplayName(agent.displayName ?? "");
                  setEditingDisplayName(true);
                }}
                title="Edit display name"
                type="button"
              >
                <PencilIcon />
              </button>
            ) : null}
          </div>
        </div>
        <EditorStatusChip
          dirty={dirty}
          saved={savedStatus === `agent:${agent.agentDid}`}
        />
      </div>
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}

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
              // An abandoned rename must not leave its failure on screen.
              setSaveError(null);
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
