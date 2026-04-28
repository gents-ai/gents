import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type {
  BehaviorSaveRequest,
  BehaviorView,
  DeploymentView,
  InferenceBackendView,
  InferenceProfileView,
  ToolSelectionView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import {
  boolText,
  isOptionalFloat,
  optionalString,
  parseOptionalFloat,
} from "./formUtils";

export type BehaviorConfigPanelProps = {
  deployment: DeploymentView;
  selectedBehavior: BehaviorView | null;
  saving: boolean;
  savedStatus: string | null;
  onSavedStatusChange: (value: string) => void;
  onCreateBehavior: () => void;
  onSelectBehavior: (behaviorId: string) => void;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
};

export function BehaviorConfigPanel({
  deployment,
  selectedBehavior,
  saving,
  savedStatus,
  onSavedStatusChange,
  onCreateBehavior,
  onSelectBehavior,
  onSaveBehaviorConfig,
}: BehaviorConfigPanelProps) {
  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Documents"
        items={deployment.behaviors.map((behavior) => ({
          id: behavior.behaviorId,
          title: behavior.displayName,
          meta: behavior.isDefault ? "default" : boolText(behavior.enabled),
        }))}
        selectedId={selectedBehavior?.behaviorId ?? null}
        testPrefix="behavior"
        title="Agent Behaviors"
        onCreate={onCreateBehavior}
        onSelect={onSelectBehavior}
      />

      <BehaviorConfigEditor
        agentDid={deployment.agentDid}
        behavior={selectedBehavior}
        inferenceBackends={deployment.inferenceBackends}
        inferenceProfiles={deployment.inferenceProfiles}
        savedStatus={savedStatus}
        saving={saving}
        toolSelections={deployment.toolSelections}
        onSaved={(behaviorId) => {
          onSelectBehavior(behaviorId);
          onSavedStatusChange(`behavior:${behaviorId}`);
        }}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
      />
    </section>
  );
}

export type BehaviorConfigEditorProps = {
  agentDid: string;
  behavior: BehaviorView | null;
  inferenceBackends: InferenceBackendView[];
  inferenceProfiles: InferenceProfileView[];
  toolSelections: ToolSelectionView[];
  saving: boolean;
  savedStatus: string | null;
  onSaved: (behaviorId: string) => void;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
};

export function BehaviorConfigEditor({
  agentDid,
  behavior,
  inferenceBackends,
  inferenceProfiles,
  toolSelections,
  saving,
  savedStatus,
  onSaved,
  onSaveBehaviorConfig,
}: BehaviorConfigEditorProps) {
  const [behaviorId, setBehaviorId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [backendId, setBackendId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [toolSelectionId, setToolSelectionId] = useState("");
  const [compactionStrategy, setCompactionStrategy] =
    useState("StripThenSummarize");
  const [compactionThreshold, setCompactionThreshold] = useState("0.95");
  const [enabled, setEnabled] = useState(true);

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
    setDisplayName(behavior?.displayName ?? "");
    setSystemPrompt(behavior?.systemPrompt ?? "");
    setBackendId(behavior?.backendId ?? "");
    setProfileId(nextProfileId);
    setToolSelectionId(behavior?.toolSelectionId ?? "");
    setCompactionStrategy(behavior?.compactionStrategy ?? "StripThenSummarize");
    setCompactionThreshold(
      behavior?.compactionThreshold != null
        ? String(behavior.compactionThreshold)
        : "0.95",
    );
    setEnabled(behavior?.enabled ?? true);
  }, [behavior, inferenceProfiles]);

  const compactionThresholdValid = isOptionalFloat(compactionThreshold, {
    min: 0,
    max: 1,
  });

  async function submitBehavior(event: FormEvent) {
    event.preventDefault();
    const nextId = behaviorId.trim();
    await onSaveBehaviorConfig({
      agentDid,
      behaviorId: nextId,
      displayName,
      systemPrompt,
      backendId: optionalString(backendId),
      inferenceProfileId: profileId.trim(),
      toolSelectionId: optionalString(toolSelectionId),
      compactionStrategy: optionalString(compactionStrategy),
      compactionThreshold: parseOptionalFloat(compactionThreshold),
      enabled,
    });
    onSaved(nextId);
  }

  return (
    <form className="panel config-editor" onSubmit={submitBehavior}>
      <ConfigEditorHeader
        eyebrow="Behavior"
        saved={savedStatus === `behavior:${behaviorId.trim()}`}
        title={displayName || "New Behavior"}
      />

      <div className="grid-2">
        <label className="field">
          <span>Behavior key</span>
          <input
            data-testid="behavior-id"
            onChange={(event) => setBehaviorId(event.currentTarget.value)}
            value={behaviorId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="behavior-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>

      <div className="grid-3">
        <label className="field">
          <span>Backend</span>
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
        </label>
        <label className="field">
          <span>Profile</span>
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
        </label>
        <label className="field">
          <span>Tool selection</span>
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
        </label>
      </div>

      <div className="grid-3">
        <label className="field">
          <span>Compaction strategy</span>
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
          <span>Compaction threshold</span>
          <input
            data-testid="behavior-compaction-threshold"
            max="1"
            min="0"
            onChange={(event) => setCompactionThreshold(event.currentTarget.value)}
            step="0.01"
            type="number"
            value={compactionThreshold}
          />
        </label>
        <label className="checkbox">
          <input
            checked={enabled}
            data-testid="behavior-enabled"
            onChange={(event) => setEnabled(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Enabled</span>
        </label>
      </div>

      <div className="facts facts-single">
        <div>
          <dt>Resolved model</dt>
          <dd>{behavior?.modelName ?? "not resolved"}</dd>
        </div>
      </div>

      <label className="field">
        <span>System prompt</span>
        <textarea
          className="config-textarea"
          data-testid="behavior-system-prompt"
          onChange={(event) => setSystemPrompt(event.currentTarget.value)}
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
            !displayName.trim() ||
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
