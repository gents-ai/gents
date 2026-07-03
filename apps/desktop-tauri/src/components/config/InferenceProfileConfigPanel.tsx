import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  InferenceProfileSaveRequest,
  InferenceProfileView,
} from "../../lib/types";
import { ConfigDocumentList, ConfigEditorHeader } from "./ConfigChrome";
import {
  ignoreHandledActionError,
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
  onSaveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<unknown>;
};

export function InferenceProfileConfigPanel({
  deployment,
  selectedProfileId,
  saving,
  savedStatus,
  onSelectProfile,
  onCreateProfile,
  onSavedStatusChange,
  onSaveInferenceProfileConfig,
}: InferenceProfileConfigPanelProps) {
  const selectedProfile = useMemo(
    () =>
      deployment.inferenceProfiles.find(
        (profile) => profile.profileId === selectedProfileId,
      ) ?? null,
    [deployment.inferenceProfiles, selectedProfileId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Inference"
        items={deployment.inferenceProfiles.map((profile) => ({
          id: profile.profileId,
          title: profile.displayName ?? profile.profileId,
          meta:
            profile.maxOutputTokens != null
              ? `${profile.maxOutputTokens} max output`
              : "profile",
        }))}
        selectedId={selectedProfileId}
        testPrefix="profile"
        title="Inference Profiles"
        onCreate={onCreateProfile}
        onSelect={onSelectProfile}
      />

      <InferenceProfileConfigEditor
        profile={selectedProfile}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(profileId) => {
          onSelectProfile(profileId);
          onSavedStatusChange(`profile:${profileId}`);
        }}
        onSaveInferenceProfileConfig={onSaveInferenceProfileConfig}
      />
    </section>
  );
}

export type InferenceProfileConfigEditorProps = {
  profile: InferenceProfileView | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (profileId: string) => void;
  onSaveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<unknown>;
};

export function InferenceProfileConfigEditor({
  profile,
  savedStatus,
  saving,
  onSaved,
  onSaveInferenceProfileConfig,
}: InferenceProfileConfigEditorProps) {
  const [profileId, setProfileId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [contextWindow, setContextWindow] = useState("");
  const [maxOutputTokens, setMaxOutputTokens] = useState("");
  const [maxTurns, setMaxTurns] = useState("");
  const [temperature, setTemperature] = useState("");
  const [streamBatchMs, setStreamBatchMs] = useState("");
  const [streamLivenessSecs, setStreamLivenessSecs] = useState("");
  const [deadlineSecs, setDeadlineSecs] = useState("");

  useEffect(() => {
    setProfileId(profile?.profileId ?? "");
    setDisplayName(profile?.displayName ?? profile?.profileId ?? "");
    setContextWindow(
      profile?.contextWindow != null ? String(profile.contextWindow) : "",
    );
    setMaxOutputTokens(
      profile?.maxOutputTokens != null ? String(profile.maxOutputTokens) : "",
    );
    setMaxTurns(profile?.maxTurns != null ? String(profile.maxTurns) : "");
    setTemperature(profile?.temperature != null ? String(profile.temperature) : "");
    setStreamBatchMs(
      profile?.streamBatchMs != null ? String(profile.streamBatchMs) : "",
    );
    setStreamLivenessSecs(
      profile?.streamLivenessTimeoutSecs != null
        ? String(profile.streamLivenessTimeoutSecs)
        : "",
    );
    setDeadlineSecs(
      profile?.deadlineDurationSecs != null ? String(profile.deadlineDurationSecs) : "",
    );
    // Id-keyed: background snapshot refreshes must not wipe in-progress edits.
  }, [profile?.profileId]);

  const contextWindowValid = isOptionalInt(contextWindow, { min: 1 });
  const maxOutputTokensValid = isOptionalInt(maxOutputTokens, { min: 1 });
  const maxTurnsValid = isOptionalInt(maxTurns, { min: 1 });
  const temperatureValid = isOptionalFloat(temperature, { min: 0 });
  const streamBatchValid = isOptionalInt(streamBatchMs, { min: 0 });
  const streamLivenessValid = isOptionalInt(streamLivenessSecs, { min: 1 });
  const deadlineValid = isOptionalInt(deadlineSecs, { min: 1 });

  async function submitProfile(event: FormEvent) {
    event.preventDefault();
    const nextId = profileId.trim();
    try {
      await onSaveInferenceProfileConfig({
        profileId: nextId,
        displayName,
        contextWindow: parseOptionalInt(contextWindow),
        maxOutputTokens: parseOptionalInt(maxOutputTokens),
        maxTurns: parseOptionalInt(maxTurns),
        temperature: parseOptionalFloat(temperature),
        streamBatchMs: parseOptionalInt(streamBatchMs),
        streamLivenessTimeoutSecs: parseOptionalInt(streamLivenessSecs),
        deadlineDurationSecs: parseOptionalInt(deadlineSecs),
      });
      onSaved(nextId);
    } catch (error) {
      ignoreHandledActionError(error);
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitProfile}>
      <ConfigEditorHeader
        eyebrow="Profile"
        saved={savedStatus === `profile:${profileId.trim()}`}
        title={displayName || profileId || "New Profile"}
      />
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
        </label>
        <label className="field">
          <span>Max output tokens</span>
          <input
            data-testid="profile-max-output-tokens"
            onChange={(event) => setMaxOutputTokens(event.currentTarget.value)}
            type="number"
            value={maxOutputTokens}
          />
        </label>
        <label className="field">
          <span>Max turns</span>
          <input
            data-testid="profile-max-turns"
            onChange={(event) => setMaxTurns(event.currentTarget.value)}
            type="number"
            value={maxTurns}
          />
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
        </label>
        <label className="field">
          <span>Stream batch ms</span>
          <input
            data-testid="profile-stream-batch-ms"
            onChange={(event) => setStreamBatchMs(event.currentTarget.value)}
            type="number"
            value={streamBatchMs}
          />
        </label>
        <label className="field">
          <span>Stream liveness seconds</span>
          <input
            data-testid="profile-stream-liveness-timeout-secs"
            onChange={(event) => setStreamLivenessSecs(event.currentTarget.value)}
            type="number"
            value={streamLivenessSecs}
          />
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
        </label>
      </div>
      <div className="config-actions">
        <button
          className="primary-button"
          data-testid="profile-save"
          disabled={
            saving ||
            !profileId.trim() ||
            !displayName.trim() ||
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
