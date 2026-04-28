import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  AgentConfigSaveRequest,
  AgentPrincipalView,
  BehaviorView,
  BootstrapSummary,
  DeploymentView,
} from "../../lib/types";
import { ConfigEditorHeader } from "./ConfigChrome";

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
  const [defaultBehaviorId, setDefaultBehaviorId] = useState("");
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    setDisplayName(agent.displayName ?? "");
    setDefaultBehaviorId(
      agent.defaultBehaviorId ??
        behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
        behaviors[0]?.behaviorId ??
        "",
    );
    setEnabled(agent.enabled ?? true);
  }, [agent, behaviors]);

  async function submitAgent(event: FormEvent) {
    event.preventDefault();
    await onSaveAgentConfig({
      agentDid: agent.agentDid,
      displayName,
      defaultBehaviorId,
      enabled,
    });
    onSaved(agent.agentDid);
  }

  return (
    <form className="panel config-editor" onSubmit={submitAgent}>
      <ConfigEditorHeader
        eyebrow="Agent"
        saved={savedStatus === `agent:${agent.agentDid}`}
        title={displayName || "Agent"}
      />

      <div className="facts">
        <div>
          <dt>Agent DID</dt>
          <dd className="mono">{agent.agentDid}</dd>
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
          <dd className="mono">{bootstrap?.initToolRoot ?? "not configured"}</dd>
        </div>
      </div>

      <div className="grid-2">
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="agent-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
        <label className="field">
          <span>Default behavior</span>
          <select
            data-testid="agent-default-behavior-id"
            onChange={(event) => setDefaultBehaviorId(event.currentTarget.value)}
            value={defaultBehaviorId}
          >
            {!behaviors.length ? (
              <option value="">No behaviors available</option>
            ) : null}
            {behaviors.map((behavior) => (
              <option key={behavior.behaviorId} value={behavior.behaviorId}>
                {behavior.displayName}
              </option>
            ))}
          </select>
        </label>
      </div>

      <label className="checkbox">
        <input
          checked={enabled}
          data-testid="agent-enabled"
          onChange={(event) => setEnabled(event.currentTarget.checked)}
          type="checkbox"
        />
        <span>Enabled</span>
      </label>

      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="agent-save"
          disabled={saving || !displayName.trim() || !defaultBehaviorId.trim()}
          type="submit"
        >
          {saving ? "Saving..." : "Save Agent"}
        </button>
      </div>
    </form>
  );
}
