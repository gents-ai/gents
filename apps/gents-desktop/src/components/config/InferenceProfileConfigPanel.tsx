import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  InferenceProfileDeleteRequest,
  ConfigComponentsApplyRequest,
  InferenceSampling,
  InferenceExecution,
  InferenceProfile,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import {
  isOptionalFloat,
  isOptionalInt,
  parseOptionalFloat,
  parseOptionalInt,
} from "./formUtils";

export type InferenceProfileConfigPanelProps = {
  deployment: DeploymentView;
  selectedProfileId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectProfile: (profileId: string) => void;
  onCreateProfile: () => void;
  onSavedStatusChange: (value: string) => void;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteInferenceProfileConfig: (
    request: InferenceProfileDeleteRequest,
  ) => Promise<unknown>;
  onDeletedProfile: () => void;
};

export function InferenceProfileConfigPanel({
  deployment,
  selectedProfileId,
  saving,
  savedStatus,
  onSelectProfile,
  onCreateProfile,
  onSavedStatusChange,
  onApplyConfigComponents,
  onDeleteInferenceProfileConfig,
  onDeletedProfile,
}: InferenceProfileConfigPanelProps) {
  const selectedProfile = useMemo(
    () =>
      deployment.inferenceProfiles.find(
        (profile) => profile.profile_id === selectedProfileId,
      ) ?? null,
    [deployment.inferenceProfiles, selectedProfileId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Inference"
        items={deployment.inferenceProfiles.map((profile) => ({
          id: profile.profile_id,
          title: profile.display_name ?? profile.profile_id,
          meta:
            profile.max_output_tokens != null
              ? `${profile.max_output_tokens} max output`
              : "profile",
        }))}
        selectedId={selectedProfileId}
        testPrefix="profile"
        title="Inference Profiles"
        onCreate={onCreateProfile}
        onSelect={onSelectProfile}
      />

      <InferenceProfileConfigEditor
        agentDid={deployment.agentDid}
        profile={selectedProfile}
        samplingConfigs={deployment.inferenceSampling}
        executionConfigs={deployment.inferenceExecution}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(profileId) => {
          onSelectProfile(profileId);
          onSavedStatusChange(`profile:${profileId}`);
        }}
        onApplyConfigComponents={onApplyConfigComponents}
        onDeleteInferenceProfileConfig={onDeleteInferenceProfileConfig}
        onDeleted={() => {
          onDeletedProfile();
        }}
      />
    </section>
  );
}

export type InferenceProfileConfigEditorProps = {
  agentDid: string;
  profile: InferenceProfile | null;
  samplingConfigs: InferenceSampling[];
  executionConfigs: InferenceExecution[];
  savedStatus: string | null;
  saving: boolean;
  onSaved: (profileId: string) => void;
  onApplyConfigComponents: (request: ConfigComponentsApplyRequest) => Promise<unknown>;
  onDeleteInferenceProfileConfig: (
    request: InferenceProfileDeleteRequest,
  ) => Promise<unknown>;
  onDeleted: () => void;
};

export function InferenceProfileConfigEditor({
  agentDid,
  profile,
  samplingConfigs,
  executionConfigs,
  savedStatus,
  saving,
  onSaved,
  onApplyConfigComponents,
  onDeleteInferenceProfileConfig,
  onDeleted,
}: InferenceProfileConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteInferenceProfile() {
    setConfirmingDelete(false);
    if (!profile) {
      return;
    }
    try {
      await onDeleteInferenceProfileConfig({
        profileId: profile.profile_id,
        agentDid,
      });
      onDeleted();
    } catch {}
  }
  const [profileId, setProfileId] = useState("");
  const [backendId, setBackendId] = useState("");
  const [modelName, setModelName] = useState("");
  const [samplingId, setSamplingId] = useState("");
  const [executionId, setExecutionId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [contextWindow, setContextWindow] = useState("");
  const [maxOutputTokens, setMaxOutputTokens] = useState("");
  const [maxTurns, setMaxTurns] = useState("");
  const [temperature, setTemperature] = useState("");
  const [streamBatchMs, setStreamBatchMs] = useState("");
  const [streamLivenessSecs, setStreamLivenessSecs] = useState("");
  const [deadlineSecs, setDeadlineSecs] = useState("");

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const b = profileFormValues(profile, samplingConfigs, executionConfigs);
    setProfileId(b.profileId);
    setBackendId(b.backendId);
    setModelName(b.modelName);
    setSamplingId(b.samplingId);
    setExecutionId(b.executionId);
    setDisplayName(b.displayName);
    setContextWindow(b.contextWindow);
    setMaxOutputTokens(b.maxOutputTokens);
    setMaxTurns(b.maxTurns);
    setTemperature(b.temperature);
    setStreamBatchMs(b.streamBatchMs);
    setStreamLivenessSecs(b.streamLivenessSecs);
    setDeadlineSecs(b.deadlineSecs);
    setSaveError(null);
  }, [profile?.profile_id]);

  const contextWindowValid = isOptionalInt(contextWindow, { min: 1 });
  const maxOutputTokensValid = isOptionalInt(maxOutputTokens, { min: 1 });
  const maxTurnsValid = isOptionalInt(maxTurns, { min: 1 });
  const temperatureValid = isOptionalFloat(temperature, { min: 0 });
  const streamBatchValid = isOptionalInt(streamBatchMs, { min: 0 });
  const streamLivenessValid = isOptionalInt(streamLivenessSecs, { min: 1 });
  const deadlineValid = isOptionalInt(deadlineSecs, { min: 1 });

  async function submitProfile(event: FormEvent) {
    event.preventDefault();
    const nextId = profile?.profile_id ?? profileId.trim();
    try {
      if (!samplingId && temperature !== "")
        throw new Error("Choose a sampling document ID to configure temperature.");
      if (
        !executionId &&
        [maxTurns, streamBatchMs, streamLivenessSecs, deadlineSecs].some(
          (value) => value !== "",
        )
      )
        throw new Error(
          "Choose an execution document ID to configure execution limits.",
        );
      const sampling = samplingConfigs.find(
        (entry) => entry.sampling_id === samplingId,
      );
      const execution = executionConfigs.find(
        (entry) => entry.execution_id === executionId,
      );
      await onApplyConfigComponents({
        document: {
          agent_principal: { agent_did: agentDid },
          inference_profiles: [
            {
              ...profile,
              agent_did: agentDid,
              profile_id: nextId,
              backend_id: backendId,
              model_name: modelName,
              sampling_id: samplingId || null,
              execution_id: executionId || null,
              display_name: displayName || null,
              context_window: parseOptionalInt(contextWindow),
              max_output_tokens: parseOptionalInt(maxOutputTokens),
            },
          ],
          inference_sampling: samplingId
            ? [
                {
                  ...sampling,
                  agent_did: agentDid,
                  sampling_id: samplingId,
                  temperature: parseOptionalFloat(temperature),
                },
              ]
            : [],
          inference_execution: executionId
            ? [
                {
                  ...execution,
                  agent_did: agentDid,
                  execution_id: executionId,
                  max_turns: parseOptionalInt(maxTurns),
                  stream_batch_ms: parseOptionalInt(streamBatchMs),
                  stream_liveness_timeout_secs: parseOptionalInt(streamLivenessSecs),
                  deadline_duration_secs: parseOptionalInt(deadlineSecs),
                },
              ]
            : [],
        },
      });
      onSaved(nextId);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitProfile}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            profileId,
            backendId,
            modelName,
            samplingId,
            executionId,
            displayName,
            contextWindow,
            maxOutputTokens,
            maxTurns,
            temperature,
            streamBatchMs,
            streamLivenessSecs,
            deadlineSecs,
          },
          profileFormValues(profile, samplingConfigs, executionConfigs),
        )}
        eyebrow="Profile"
        saved={savedStatus === `profile:${profileId.trim()}`}
        title={displayName || profileId || "New Profile"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Backend ID</span>
          <input
            data-testid="profile-backend-id"
            value={backendId}
            onChange={(event) => setBackendId(event.currentTarget.value)}
          />
        </label>
        <label className="field">
          <span>Model</span>
          <input
            data-testid="profile-model-name"
            value={modelName}
            onChange={(event) => setModelName(event.currentTarget.value)}
          />
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Sampling document ID</span>
          <input
            data-testid="profile-sampling-id"
            value={samplingId}
            onChange={(event) => {
              const id = event.currentTarget.value;
              setSamplingId(id);
              const selected = samplingConfigs.find(
                (entry) => entry.sampling_id === id,
              );
              setTemperature(
                selected?.temperature == null ? "" : String(selected.temperature),
              );
            }}
          />
          <span>Reuse an existing ID or name a new sampling document.</span>
        </label>
        <label className="field">
          <span>Execution document ID</span>
          <input
            data-testid="profile-execution-id"
            value={executionId}
            onChange={(event) => {
              const id = event.currentTarget.value;
              setExecutionId(id);
              const selected = executionConfigs.find(
                (entry) => entry.execution_id === id,
              );
              setMaxTurns(
                selected?.max_turns == null ? "" : String(selected.max_turns),
              );
              setStreamBatchMs(
                selected?.stream_batch_ms == null
                  ? ""
                  : String(selected.stream_batch_ms),
              );
              setStreamLivenessSecs(
                selected?.stream_liveness_timeout_secs == null
                  ? ""
                  : String(selected.stream_liveness_timeout_secs),
              );
              setDeadlineSecs(
                selected?.deadline_duration_secs == null
                  ? ""
                  : String(selected.deadline_duration_secs),
              );
            }}
          />
          <span>Shared policies affect every profile that references them.</span>
        </label>
      </div>
      <div className="grid-2">
        <label className="field">
          <span>Profile document ID</span>
          <input
            data-testid="profile-id"
            onChange={(event) => {
              if (!profile) {
                setProfileId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(profile)}
            title={
              profile ? "Profile IDs cannot be renamed after creation." : undefined
            }
            value={profileId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="profile-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Context window</span>
          <input
            data-testid="profile-context-window"
            onChange={(event) => setContextWindow(event.currentTarget.value)}
            type="number"
            value={contextWindow}
          />
          <FieldHint show={!contextWindowValid}>Whole number of 1 or more</FieldHint>
        </label>
        <label className="field">
          <span>Max output tokens</span>
          <input
            data-testid="profile-max-output-tokens"
            onChange={(event) => setMaxOutputTokens(event.currentTarget.value)}
            type="number"
            value={maxOutputTokens}
          />
          <FieldHint show={!maxOutputTokensValid}>Whole number of 1 or more</FieldHint>
        </label>
        <label className="field">
          <span>Max turns</span>
          <input
            data-testid="profile-max-turns"
            onChange={(event) => setMaxTurns(event.currentTarget.value)}
            type="number"
            value={maxTurns}
          />
          <FieldHint show={!maxTurnsValid}>Whole number of 1 or more</FieldHint>
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Temperature</span>
          <input
            data-testid="profile-temperature"
            onChange={(event) => setTemperature(event.currentTarget.value)}
            step="0.01"
            type="number"
            value={temperature}
          />
          <FieldHint show={!temperatureValid}>Number of 0 or more</FieldHint>
        </label>
        <label className="field">
          <span>Stream batch ms</span>
          <input
            data-testid="profile-stream-batch-ms"
            onChange={(event) => setStreamBatchMs(event.currentTarget.value)}
            type="number"
            value={streamBatchMs}
          />
          <FieldHint show={!streamBatchValid}>Whole number of 0 or more</FieldHint>
        </label>
        <label className="field">
          <span>Stream liveness seconds</span>
          <input
            data-testid="profile-stream-liveness-timeout-secs"
            onChange={(event) => setStreamLivenessSecs(event.currentTarget.value)}
            type="number"
            value={streamLivenessSecs}
          />
          <FieldHint show={!streamLivenessValid}>Whole number of 1 or more</FieldHint>
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Deadline seconds</span>
          <input
            data-testid="profile-deadline-duration-secs"
            onChange={(event) => setDeadlineSecs(event.currentTarget.value)}
            type="number"
            value={deadlineSecs}
          />
          <FieldHint show={!deadlineValid}>Whole number of 1 or more</FieldHint>
        </label>
      </div>
      <div className="config-actions">
        {profile ? (
          <button
            className="ghost-button danger-button"
            data-testid="profile-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Profile
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete profile"
          message={`Delete profile "${profile?.profile_id ?? ""}"? Behaviors still pointing at it will block the delete.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            void deleteInferenceProfile();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="profile-save"
          disabled={
            saving ||
            !profileId.trim() ||
            !backendId.trim() ||
            !modelName.trim() ||
            !contextWindowValid ||
            !maxOutputTokensValid ||
            !maxTurnsValid ||
            !temperatureValid ||
            !streamBatchValid ||
            !streamLivenessValid ||
            !deadlineValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Profile"}
        </button>
      </div>
    </form>
  );
}

function profileFormValues(
  profile: InferenceProfile | null,
  samplingConfigs: InferenceSampling[],
  executionConfigs: InferenceExecution[],
) {
  const sampling = samplingConfigs.find(
    (entry) => entry.sampling_id === profile?.sampling_id,
  );
  const execution = executionConfigs.find(
    (entry) => entry.execution_id === profile?.execution_id,
  );
  const text = (value: number | null | undefined) =>
    value == null ? "" : String(value);
  return {
    profileId: profile?.profile_id ?? "",
    backendId: profile?.backend_id ?? "",
    modelName: profile?.model_name ?? "",
    samplingId: profile?.sampling_id ?? "",
    executionId: profile?.execution_id ?? "",
    displayName: profile?.display_name ?? "",
    contextWindow: text(profile?.context_window),
    maxOutputTokens: text(profile?.max_output_tokens),
    maxTurns: text(execution?.max_turns),
    temperature: text(sampling?.temperature),
    streamBatchMs: text(execution?.stream_batch_ms),
    streamLivenessSecs: text(execution?.stream_liveness_timeout_secs),
    deadlineSecs: text(execution?.deadline_duration_secs),
  };
}
