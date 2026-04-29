import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  AgentConfigSaveRequest,
  BehaviorSaveRequest,
  BehaviorView,
  DeploymentView,
  InferenceBackendView,
  InferenceProfileView,
  ToolSelectionView,
} from "../../lib/types";
import { ConfigDocumentList, PencilIcon, PlusIcon } from "./ConfigChrome";
import {
  boolText,
  isOptionalFloat,
  optionalString,
  parseOptionalFloat,
} from "./formUtils";

const DEFAULT_COMPACTION_STRATEGY = "StripThenSummarize";
const DEFAULT_COMPACTION_THRESHOLD = "0.75";

export type BehaviorConfigPanelProps = {
  deployment: DeploymentView;
  selectedBehavior: BehaviorView | null;
  saving: boolean;
  savedStatus: string | null;
  onSavedStatusChange: (value: string) => void;
  onCreateBehavior: () => void;
  onCreateBackend: () => void;
  onCreateProfile: () => void;
  onCreateToolSelection: () => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
};

export function BehaviorConfigPanel({
  deployment,
  selectedBehavior,
  saving,
  savedStatus,
  onSavedStatusChange,
  onCreateBehavior,
  onCreateBackend,
  onCreateProfile,
  onCreateToolSelection,
  onSelectBehavior,
  onSaveAgentConfig,
  onSaveBehaviorConfig,
}: BehaviorConfigPanelProps) {
  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Documents"
        items={deployment.behaviors.map((behavior) => ({
          id: behavior.behaviorId,
          title: behavior.behaviorId,
          meta: behavior.isDefault ? "default" : boolText(behavior.enabled),
        }))}
        selectedId={selectedBehavior?.behaviorId ?? null}
        testPrefix="behavior"
        title="Agent Behaviors"
        onCreate={onCreateBehavior}
        onSelect={onSelectBehavior}
      />

      <BehaviorConfigEditor
        agentDisplayName={
          deployment.agentPrincipal.displayName ?? deployment.label ?? "Agent"
        }
        agentDid={deployment.agentDid}
        agentEnabled={deployment.agentPrincipal.enabled ?? true}
        behavior={selectedBehavior}
        currentDefaultBehaviorId={
          deployment.defaultBehaviorId ??
          deployment.agentPrincipal.defaultBehaviorId ??
          null
        }
        inferenceBackends={deployment.inferenceBackends}
        inferenceProfiles={deployment.inferenceProfiles}
        savedStatus={savedStatus}
        saving={saving}
        toolSelections={deployment.toolSelections}
        onCreateBackend={onCreateBackend}
        onCreateProfile={onCreateProfile}
        onCreateToolSelection={onCreateToolSelection}
        onSaved={(behaviorId) => {
          onSelectBehavior(behaviorId);
          onSavedStatusChange(`behavior:${behaviorId}`);
        }}
        onSaveAgentConfig={onSaveAgentConfig}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
      />
    </section>
  );
}

export type BehaviorConfigEditorProps = {
  agentDisplayName: string;
  agentDid: string;
  agentEnabled: boolean;
  behavior: BehaviorView | null;
  currentDefaultBehaviorId: string | null;
  inferenceBackends: InferenceBackendView[];
  inferenceProfiles: InferenceProfileView[];
  toolSelections: ToolSelectionView[];
  saving: boolean;
  savedStatus: string | null;
  onCreateBackend: () => void;
  onCreateProfile: () => void;
  onCreateToolSelection: () => void;
  onSaved: (behaviorId: string) => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
};

export function BehaviorConfigEditor({
  agentDisplayName,
  agentDid,
  agentEnabled,
  behavior,
  currentDefaultBehaviorId,
  inferenceBackends,
  inferenceProfiles,
  toolSelections,
  saving,
  savedStatus,
  onCreateBackend,
  onCreateProfile,
  onCreateToolSelection,
  onSaved,
  onSaveAgentConfig,
  onSaveBehaviorConfig,
}: BehaviorConfigEditorProps) {
  const [behaviorId, setBehaviorId] = useState("");
  const [editingBehaviorId, setEditingBehaviorId] = useState(false);
  const [systemPrompt, setSystemPrompt] = useState("");
  const [backendId, setBackendId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [toolSelectionId, setToolSelectionId] = useState("");
  const [compactionStrategy, setCompactionStrategy] =
    useState(DEFAULT_COMPACTION_STRATEGY);
  const [compactionThreshold, setCompactionThreshold] = useState(
    DEFAULT_COMPACTION_THRESHOLD,
  );
  const [enabled, setEnabled] = useState(true);
  const [defaultForAgent, setDefaultForAgent] = useState(false);

  useEffect(() => {
    const selectedProfileId = behavior?.inferenceProfileId ?? null;
    let nextProfileId = inferenceProfiles[0]?.profileId ?? "";
    if (
      selectedProfileId &&
      inferenceProfiles.some((profile) => profile.profileId === selectedProfileId)
    ) {
      nextProfileId = selectedProfileId;
    }
    setBehaviorId(behavior?.behaviorId ?? "");
    setEditingBehaviorId(!behavior);
    setSystemPrompt(behavior?.systemPrompt ?? "");
    setBackendId(behavior?.backendId ?? "");
    setProfileId(nextProfileId);
    setToolSelectionId(behavior?.toolSelectionId ?? "");
    setCompactionStrategy(behavior?.compactionStrategy ?? DEFAULT_COMPACTION_STRATEGY);
    setCompactionThreshold(
      behavior?.compactionThreshold != null
        ? String(behavior.compactionThreshold)
        : DEFAULT_COMPACTION_THRESHOLD,
    );
    setEnabled(behavior?.enabled ?? true);
    setDefaultForAgent(behavior?.isDefault ?? false);
  }, [behavior, inferenceProfiles]);

  const selectedBackend = inferenceBackends.find(
    (backend) => backend.backendId === backendId,
  );
  const backendModels = selectedBackend?.models.filter(Boolean) ?? [];
  const resolvedModel = backendModels.length
    ? backendModels.join(", ")
    : selectedBackend
      ? "No models configured"
      : "Select a backend";
  const promptRows = Math.min(
    28,
    Math.max(
      14,
      systemPrompt.split("\n").length + Math.ceil(systemPrompt.length / 90),
    ),
  );
  const compactionThresholdValid =
    isOptionalFloat(compactionThreshold, {
      min: 0,
      max: 1,
    });

  async function submitBehavior(event: FormEvent) {
    event.preventDefault();
    const nextId = behaviorId.trim();
    await onSaveBehaviorConfig({
      agentDid,
      behaviorId: nextId,
      displayName: nextId,
      systemPrompt,
      backendId: optionalString(backendId),
      inferenceProfileId: profileId.trim(),
      toolSelectionId: optionalString(toolSelectionId),
      compactionStrategy: optionalString(compactionStrategy),
      compactionThreshold: parseOptionalFloat(compactionThreshold),
      enabled,
    });
    if (defaultForAgent && nextId !== currentDefaultBehaviorId) {
      await onSaveAgentConfig({
        agentDid,
        displayName: agentDisplayName,
        defaultBehaviorId: nextId,
        enabled: agentEnabled,
      });
    }
    onSaved(nextId);
  }

  return (
    <form className="panel config-editor behavior-config-editor" onSubmit={submitBehavior}>
      <div className="panel-header behavior-config-header">
        <div>
          <p className="eyebrow">Behavior</p>
          <div className="behavior-key-row">
            {editingBehaviorId ? (
              <input
                className="behavior-key-input"
                data-testid="behavior-id"
                onChange={(event) => setBehaviorId(event.currentTarget.value)}
                value={behaviorId}
              />
            ) : (
              <h3>{behaviorId || "New Behavior"}</h3>
            )}
            {!editingBehaviorId ? (
              <button
                aria-label="Edit behavior key"
                className="ghost-button config-icon-button"
                data-testid="behavior-edit-key"
                onClick={() => setEditingBehaviorId(true)}
                title="Edit behavior key"
                type="button"
              >
                <PencilIcon />
              </button>
            ) : null}
          </div>
        </div>
        {savedStatus === `behavior:${behaviorId.trim()}` ? (
          <span className="chip chip-green">Saved</span>
        ) : null}
      </div>

      <div className="behavior-link-grid">
        <label className="field behavior-link-field">
          <span>Backend</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-backend-id"
              onChange={(event) => setBackendId(event.currentTarget.value)}
              value={backendId}
            >
              <option value="">Unset</option>
              {inferenceBackends.map((backend) => (
                <option key={backend.backendId} value={backend.backendId}>
                  {backend.name ?? backend.backendId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create backend"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-backend"
              onClick={onCreateBackend}
              title="Create backend"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
        <label className="field behavior-link-field">
          <span>Profile</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-profile-id"
              onChange={(event) => setProfileId(event.currentTarget.value)}
              value={profileId}
            >
              {!inferenceProfiles.length ? (
                <option value="">No profiles available</option>
              ) : null}
              {inferenceProfiles.map((profile) => (
                <option key={profile.profileId} value={profile.profileId}>
                  {profile.displayName ?? profile.profileId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create inference profile"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-profile"
              onClick={onCreateProfile}
              title="Create inference profile"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
        <label className="field behavior-link-field">
          <span>Tool selection</span>
          <div className="behavior-link-control">
            <select
              data-testid="behavior-tool-selection-id"
              onChange={(event) => setToolSelectionId(event.currentTarget.value)}
              value={toolSelectionId}
            >
              <option value="">Unset</option>
              {toolSelections.map((selection) => (
                <option key={selection.selectionId} value={selection.selectionId}>
                  {selection.displayName ?? selection.selectionId}
                </option>
              ))}
            </select>
            <button
              aria-label="Create tool selection"
              className="ghost-button config-icon-button"
              data-testid="behavior-create-tool-selection"
              onClick={onCreateToolSelection}
              title="Create tool selection"
              type="button"
            >
              <PlusIcon />
            </button>
          </div>
        </label>
      </div>

      <div className="grid-2 behavior-state-grid">
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="behavior-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
        <label className="checkbox">
          <input
            checked={defaultForAgent}
            data-testid="behavior-default-for-agent"
            disabled={behavior?.isDefault ?? false}
            onChange={(event) => setDefaultForAgent(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Default behavior</span>
        </label>
      </div>

      <section className="behavior-compaction-box">
        <div className="grid-2">
          <label className="field">
            <span>Strategy</span>
            <select
              data-testid="behavior-compaction-strategy"
              onChange={(event) => setCompactionStrategy(event.currentTarget.value)}
              value={compactionStrategy}
            >
              <option value="StripThenSummarize">Strip then summarize</option>
              <option value="StripToolResults">Strip tool results</option>
              <option value="Summarize">Summarize</option>
            </select>
          </label>
          <label className="field">
            <span>Threshold</span>
            <input
              data-testid="behavior-compaction-threshold"
              max="1"
              min="0"
              onChange={(event) =>
                setCompactionThreshold(event.currentTarget.value)
              }
              step="0.01"
              type="number"
              value={compactionThreshold}
            />
          </label>
        </div>
      </section>

      <div className="facts facts-single">
        <div>
          <dt>Backend model</dt>
          <dd>{resolvedModel}</dd>
        </div>
      </div>

      <label className="field">
        <span>System prompt</span>
        <textarea
          className="config-textarea behavior-system-prompt"
          data-testid="behavior-system-prompt"
          onChange={(event) => setSystemPrompt(event.currentTarget.value)}
          rows={promptRows}
          value={systemPrompt}
        />
      </label>

      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="behavior-save"
          disabled={
            saving ||
            !behaviorId.trim() ||
            !systemPrompt.trim() ||
            !profileId.trim() ||
            !compactionThresholdValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Behavior"}
        </button>
      </div>
    </form>
  );
}
