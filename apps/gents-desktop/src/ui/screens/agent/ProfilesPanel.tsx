import { useEffect, useRef, useState } from "react";
import type {
  BackendProviderKind,
  DeploymentView,
  InferenceExecution,
  InferenceModelRecommendation,
  InferenceProfile,
  InferenceSampling,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
} from "./editors";
import {
  newId,
  optionalInteger,
  optionalNumber,
  str,
  toLines,
  fromLinesOrNull,
  useDraft,
} from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import {
  InferenceModelControls,
  recommendedInferenceSettings,
  validateInferenceSettings,
  type InferenceSettingsDraft,
} from "../inference/InferenceModelControls";

function settingsForDraft(
  recommendation: InferenceModelRecommendation,
  draft: {
    contextWindow: string;
    maxOutputTokens: string;
    temperature: string;
    topP: string;
    reasoningEffort: string;
  },
  maxConcurrent: number | null | undefined,
  edited: ReadonlySet<keyof InferenceSettingsDraft> = new Set(),
): InferenceSettingsDraft {
  const defaults = recommendedInferenceSettings(recommendation);
  return {
    ...defaults,
    contextWindow: edited.has("contextWindow")
      ? draft.contextWindow
      : draft.contextWindow || defaults.contextWindow,
    maxOutputTokens: edited.has("maxOutputTokens")
      ? draft.maxOutputTokens
      : draft.maxOutputTokens || defaults.maxOutputTokens,
    temperature: edited.has("temperature")
      ? draft.temperature
      : draft.temperature || defaults.temperature,
    topP: edited.has("topP") ? draft.topP : draft.topP || defaults.topP,
    reasoningEffort: edited.has("reasoningEffort")
      ? (draft.reasoningEffort as InferenceSettingsDraft["reasoningEffort"])
      : (draft.reasoningEffort as InferenceSettingsDraft["reasoningEffort"]) ||
        defaults.reasoningEffort,
    maxConcurrent: str(maxConcurrent) || defaults.maxConcurrent,
  };
}

function Editor({
  shell,
  deployment,
  profile,
}: {
  shell: Shell;
  deployment: DeploymentView;
  profile: InferenceProfile;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "profiles",
  };
  const saved = {
    displayName: profile.display_name ?? "",
    description: profile.description ?? "",
    backendId: profile.backend_id,
    modelName: profile.model_name,
    reasoningEffort: profile.reasoning_effort ?? "",
    contextWindow: str(profile.context_window),
    maxOutputTokens: str(profile.max_output_tokens),
    samplingId: profile.sampling_id ?? "",
    temperature: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.temperature,
    ),
    topP: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.top_p,
    ),
    topK: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.top_k,
    ),
    seed: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.seed,
    ),
    minP: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.min_p,
    ),
    frequencyPenalty: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.frequency_penalty,
    ),
    presencePenalty: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.presence_penalty,
    ),
    repetitionPenalty: str(
      deployment.inferenceSampling.find(
        (row) => row.sampling_id === profile.sampling_id,
      )?.repetition_penalty,
    ),
    executionId: profile.execution_id ?? "",
    maxTurns: str(
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.max_turns,
    ),
    maxTotalTokens: str(
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.max_total_tokens,
    ),
    streamBatchMs: str(
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.stream_batch_ms,
    ),
    streamLivenessSecs: str(
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.stream_liveness_timeout_secs,
    ),
    deadlineSecs: str(
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.deadline_duration_secs,
    ),
    retryPolicyId:
      deployment.inferenceExecution.find(
        (row) => row.execution_id === profile.execution_id,
      )?.retry_policy_id ?? "",
    tags: toLines(profile.tags ?? []),
  };
  const [recommendation, setRecommendation] =
    useState<InferenceModelRecommendation | null>(null);
  const [recommendationKey, setRecommendationKey] = useState<string | null>(null);
  const [recommendationError, setRecommendationError] = useState<string | null>(null);
  const [customize, setCustomize] = useState(false);
  const [editedModelFields, setEditedModelFields] = useState<
    Set<keyof InferenceSettingsDraft>
  >(new Set());
  const deliberateSelectionRef = useRef<string | null>(null);
  const d = useDraft(saved, async (next) => {
    if (!next.backendId.trim()) throw new Error("Backend is required");
    if (!deployment.inferenceBackends.some((b) => b.backendId === next.backendId))
      throw new Error("Choose an existing backend");
    if (!next.modelName.trim()) throw new Error("Model is required");
    const modelKey = `${next.backendId}\u0000${next.modelName.trim()}`;
    if (!recommendation || recommendationKey !== modelKey)
      throw new Error(
        recommendationError ?? "Wait for model-aware settings before saving",
      );
    const selected = deployment.inferenceBackends.find(
      (backend) => backend.backendId === next.backendId,
    );
    const effectiveSettings = settingsForDraft(
      recommendation,
      next,
      selected?.maxConcurrent,
      editedModelFields,
    );
    const validationError = validateInferenceSettings(
      recommendation,
      effectiveSettings,
    );
    if (validationError) throw new Error(validationError);
    const hasSamplingValues = [
      next.temperature,
      next.topP,
      next.topK,
      next.seed,
      next.minP,
      next.frequencyPenalty,
      next.presencePenalty,
      next.repetitionPenalty,
    ].some((value) => value.trim());
    const effectiveSamplingId =
      next.samplingId.trim() ||
      (hasSamplingValues ? `${profile.profile_id}-sampling` : "");
    const executionValuesPresent = [
      next.maxTurns,
      next.maxTotalTokens,
      next.streamBatchMs,
      next.streamLivenessSecs,
      next.deadlineSecs,
      next.retryPolicyId,
    ].some((value) => value.trim());
    if (executionValuesPresent && !next.executionId.trim())
      throw new Error("Execution values require an execution document ID");

    const contextWindow = optionalInteger("Context window", next.contextWindow, {
      min: 1,
    });
    const maxOutputTokens = optionalInteger("Max output tokens", next.maxOutputTokens, {
      min: 1,
    });
    const temperature = optionalNumber("Temperature", next.temperature, {
      min: 0,
    });
    const topP = optionalNumber("Top P", next.topP, { min: 0, max: 1 });
    const topK = optionalInteger("Top K", next.topK, { min: 1 });
    const seed = optionalInteger("Seed", next.seed, { min: 0 });
    const minP = optionalNumber("Min P", next.minP, { min: 0, max: 1 });
    const frequencyPenalty = optionalNumber(
      "Frequency penalty",
      next.frequencyPenalty,
      { min: -2, max: 2 },
    );
    const presencePenalty = optionalNumber("Presence penalty", next.presencePenalty, {
      min: -2,
      max: 2,
    });
    const repetitionPenalty = optionalNumber(
      "Repetition penalty",
      next.repetitionPenalty,
      { min: Number.MIN_VALUE },
    );
    const maxTurns = optionalInteger("Max turns", next.maxTurns, { min: 1 });
    const maxTotalTokens = optionalInteger("Max total tokens", next.maxTotalTokens, {
      min: 1,
    });
    const streamBatchMs = optionalInteger("Stream batch", next.streamBatchMs, {
      min: 1,
    });
    const streamLivenessSecs = optionalInteger(
      "Stream liveness timeout",
      next.streamLivenessSecs,
      { min: 1 },
    );
    const deadlineSecs = optionalInteger("Deadline", next.deadlineSecs, { min: 1 });
    if (
      streamLivenessSecs != null &&
      deadlineSecs != null &&
      streamLivenessSecs >= deadlineSecs
    )
      throw new Error("Stream liveness timeout must be less than the deadline");

    const sampling = deployment.inferenceSampling.find(
      (row) => row.sampling_id === effectiveSamplingId,
    );
    const execution = deployment.inferenceExecution.find(
      (row) => row.execution_id === next.executionId.trim(),
    );
    const nextProfile: InferenceProfile = {
      ...profile,
      display_name: next.displayName.trim() || null,
      description: next.description.trim() || null,
      backend_id: next.backendId,
      model_name: next.modelName.trim(),
      reasoning_effort: (next.reasoningEffort || null) as NonNullable<
        InferenceProfile["reasoning_effort"]
      > | null,
      context_window: contextWindow,
      max_output_tokens: maxOutputTokens,
      sampling_id: effectiveSamplingId || null,
      execution_id: next.executionId.trim() || null,
      tags: fromLinesOrNull(next.tags),
    };
    const nextSampling: InferenceSampling | null = effectiveSamplingId
      ? {
          ...sampling,
          agent_did: deployment.agentDid,
          sampling_id: effectiveSamplingId,
          temperature,
          top_p: topP,
          top_k: topK,
          seed,
          min_p: minP,
          frequency_penalty: frequencyPenalty,
          presence_penalty: presencePenalty,
          repetition_penalty: repetitionPenalty,
        }
      : null;
    const nextExecution: InferenceExecution | null = next.executionId.trim()
      ? {
          ...execution,
          agent_did: deployment.agentDid,
          execution_id: next.executionId.trim(),
          max_turns: maxTurns,
          max_total_tokens: maxTotalTokens,
          stream_batch_ms: streamBatchMs,
          stream_liveness_timeout_secs: streamLivenessSecs,
          deadline_duration_secs: deadlineSecs,
          retry_policy_id: next.retryPolicyId.trim() || null,
        }
      : null;
    await shell.applyConfig((api) =>
      api.applyConfigComponents({
        document: {
          agent_principal: { agent_did: deployment.agentDid },
          inference_profiles: [nextProfile],
          ...(nextSampling ? { inference_sampling: [nextSampling] } : {}),
          ...(nextExecution ? { inference_execution: [nextExecution] } : {}),
        },
      }),
    );
  });
  const [executionDefaults, setExecutionDefaults] = useState<
    Record<string, number | null | undefined>
  >({});
  useEffect(() => {
    let cancelled = false;
    if (shell.api.getInferenceSetupCatalog)
      void shell.api
        .getInferenceSetupCatalog()
        .then((catalog) => {
          if (!cancelled) setExecutionDefaults(catalog.executionDefaults ?? {});
        })
        .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [shell.api]);
  const executionDefault = (key: string) =>
    executionDefaults[key] == null ? "Unlimited" : String(executionDefaults[key]);
  const [editedExecution, setEditedExecution] = useState<Set<string>>(new Set());
  const setExecution = (
    key:
      | "maxTurns"
      | "maxTotalTokens"
      | "streamBatchMs"
      | "streamLivenessSecs"
      | "deadlineSecs",
    value: string,
  ) => {
    setEditedExecution((keys) => new Set(keys).add(key));
    if (value && !d.draft.executionId)
      d.set("executionId", `${profile.profile_id}-execution`);
    d.set(key, value);
  };
  const selectedBackend = deployment.inferenceBackends.find(
    (entry) => entry.backendId === d.draft.backendId,
  );
  const advertisedModel = selectedBackend?.advertisedModels?.find(
    (entry) => entry.model_name === d.draft.modelName.trim(),
  );
  const advertisedModelKey = JSON.stringify(advertisedModel ?? null);
  const beginModelSelection = (backendId: string, modelName: string) => {
    if (
      backendId === d.draft.backendId &&
      modelName.trim() === d.draft.modelName.trim()
    )
      return;
    deliberateSelectionRef.current = `${backendId}\u0000${modelName.trim()}`;
    setRecommendation(null);
    setRecommendationKey(null);
    setRecommendationError(null);
    setEditedModelFields(new Set());
  };
  useEffect(() => {
    const backend = selectedBackend;
    const modelName = d.draft.modelName.trim();
    const requestKey = `${d.draft.backendId}\u0000${modelName}`;
    if (!backend?.providerKind || !backend.endpoint || !d.draft.modelName.trim()) {
      setRecommendation(null);
      setRecommendationKey(null);
      return;
    }
    setRecommendation(null);
    setRecommendationKey(null);
    setRecommendationError(null);
    let cancelled = false;
    const timeout = window.setTimeout(() => {
      void shell.api
        .getInferenceBackendRecommendation({
          providerKind: backend.providerKind as BackendProviderKind,
          endpoint: backend.endpoint!,
          modelName,
          displayName: advertisedModel?.display_name ?? null,
          contextWindow:
            advertisedModel?.context_window ??
            (profile.model_name === d.draft.modelName &&
            profile.backend_id === d.draft.backendId
              ? (profile.context_window ?? null)
              : null),
          maxContextWindow: advertisedModel?.max_context_window ?? null,
          maxOutputTokens:
            advertisedModel?.max_output_tokens ??
            (profile.model_name === d.draft.modelName &&
            profile.backend_id === d.draft.backendId
              ? (profile.max_output_tokens ?? null)
              : null),
          reasoningEfforts: advertisedModel?.reasoning_efforts ?? null,
        })
        .then((next) => {
          if (cancelled) return;
          const defaults = recommendedInferenceSettings(next);
          const deliberate = deliberateSelectionRef.current === requestKey;
          setRecommendation(next);
          setRecommendationKey(requestKey);
          setRecommendationError(null);
          if (deliberate) {
            d.set("contextWindow", defaults.contextWindow);
            d.set("maxOutputTokens", defaults.maxOutputTokens);
            d.set("temperature", defaults.temperature);
            d.set("topP", defaults.topP);
            d.set(
              "reasoningEffort",
              defaults.reasoningEffort as typeof d.draft.reasoningEffort,
            );
            if ((defaults.temperature || defaults.topP) && !d.draft.samplingId)
              d.set("samplingId", `${profile.profile_id}-sampling`);
            deliberateSelectionRef.current = null;
          }
        })
        .catch((error) => {
          if (cancelled) return;
          setRecommendationError(
            `Model-aware settings unavailable: ${error instanceof Error ? error.message : String(error)}`,
          );
        });
    }, 150);
    return () => {
      cancelled = true;
      window.clearTimeout(timeout);
    };
    // Draft fields are intentionally captured for the exact backend/model request.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    d.draft.backendId,
    d.draft.modelName,
    selectedBackend?.providerKind,
    selectedBackend?.endpoint,
    selectedBackend?.maxConcurrent,
    advertisedModelKey,
    shell.api,
  ]);
  const updateGuided = (next: InferenceSettingsDraft) => {
    if (guided) {
      setEditedModelFields((fields) => {
        const changed = new Set(fields);
        for (const key of [
          "contextWindow",
          "maxOutputTokens",
          "temperature",
          "topP",
          "reasoningEffort",
        ] as const) {
          if (next[key] !== guided[key]) changed.add(key);
        }
        return changed;
      });
    }
    d.set("contextWindow", next.contextWindow);
    d.set("maxOutputTokens", next.maxOutputTokens);
    d.set("temperature", next.temperature);
    d.set("topP", next.topP);
    d.set("reasoningEffort", next.reasoningEffort as typeof d.draft.reasoningEffort);
    if ((next.temperature || next.topP) && !d.draft.samplingId) {
      d.set("samplingId", `${profile.profile_id}-sampling`);
    }
  };
  const guided = recommendation
    ? settingsForDraft(
        recommendation,
        d.draft,
        selectedBackend?.maxConcurrent,
        editedModelFields,
      )
    : null;
  const id = (f: string) => `${profile.profile_id}-${f}`;
  return (
    <>
      <Group title={profile.display_name ?? profile.profile_id}>
        <FactRow label="Profile ID" mono>
          {profile.profile_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <details>
          <summary className="cursor-pointer px-4 py-3 text-sm text-muted-foreground">
            Description
          </summary>
          <AreaRow
            id={id("description")}
            label="Description"
            value={d.draft.description}
            onChange={(v) => d.set("description", v)}
            onCommit={d.commit}
            rows={2}
          />
        </details>
        <ChoiceRow
          id={id("backend")}
          label="Backend"
          value={d.draft.backendId}
          onChange={(v) => {
            const modelName =
              deployment.inferenceBackends.find((backend) => backend.backendId === v)
                ?.models[0] ?? "";
            beginModelSelection(v, modelName);
            d.set("backendId", v);
            d.set("modelName", modelName);
          }}
          items={deployment.inferenceBackends.map((b) => ({
            value: b.backendId,
            label: b.name ?? b.backendId,
          }))}
        />
        <ChoiceRow
          id={id("model")}
          label="Model"
          value={d.draft.modelName}
          onChange={(v) => {
            beginModelSelection(d.draft.backendId, v);
            d.choose("modelName", v);
          }}
          items={[
            ...new Set([
              d.draft.modelName,
              ...(deployment.inferenceBackends.find(
                (backend) => backend.backendId === d.draft.backendId,
              )?.models ?? []),
            ]),
          ]
            .filter(Boolean)
            .map((model) => ({ value: model, label: model }))}
        />
      </Group>
      {recommendation && guided ? (
        <Group title="Model-aware defaults">
          <div className="p-4">
            <InferenceModelControls
              recommendation={recommendation}
              value={guided}
              onChange={updateGuided}
              expanded={customize}
              onExpandedChange={setCustomize}
              includeConcurrency={false}
              alwaysExpanded
            />
          </div>
        </Group>
      ) : null}
      {recommendationError ? (
        <p role="alert" className="mb-4 text-sm text-destructive">
          {recommendationError}
        </p>
      ) : null}
      <Group title="Execution">
        <TextRow
          id={id("execution")}
          label="Execution document ID"
          description="Reuse an existing ID or enter a new one for these limits."
          value={d.draft.executionId}
          placeholder="Created when limits are customized"
          onChange={(v) => d.set("executionId", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <NumberRow
          id={id("max-turns")}
          label="Max turns"
          value={
            editedExecution.has("maxTurns")
              ? d.draft.maxTurns
              : d.draft.maxTurns ||
                (executionDefaults.maxTurns == null
                  ? ""
                  : String(executionDefaults.maxTurns))
          }
          onChange={(v) => setExecution("maxTurns", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("max-total")}
          label="Max total tokens"
          value={d.draft.maxTotalTokens}
          placeholder={executionDefault("maxTotalTokens")}
          onChange={(v) => setExecution("maxTotalTokens", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("batch")}
          label="Stream batch ms"
          value={
            editedExecution.has("streamBatchMs")
              ? d.draft.streamBatchMs
              : d.draft.streamBatchMs ||
                (executionDefaults.streamBatchMs == null
                  ? ""
                  : String(executionDefaults.streamBatchMs))
          }
          onChange={(v) => setExecution("streamBatchMs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("liveness")}
          label="Stream liveness seconds"
          value={
            editedExecution.has("streamLivenessSecs")
              ? d.draft.streamLivenessSecs
              : d.draft.streamLivenessSecs ||
                (executionDefaults.streamLivenessSecs == null
                  ? ""
                  : String(executionDefaults.streamLivenessSecs))
          }
          onChange={(v) => setExecution("streamLivenessSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("deadline")}
          label="Deadline seconds"
          value={
            editedExecution.has("deadlineSecs")
              ? d.draft.deadlineSecs
              : d.draft.deadlineSecs ||
                (executionDefaults.deadlineSecs == null
                  ? ""
                  : String(executionDefaults.deadlineSecs))
          }
          onChange={(v) => setExecution("deadlineSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("retry")}
          label="Retry policy ID"
          value={d.draft.retryPolicyId}
          placeholder="Runtime default"
          onChange={(v) => d.set("retryPolicyId", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
      </Group>
      <Group title="Metadata">
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={3}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={() => {
          setEditedExecution(new Set());
          setEditedModelFields(new Set());
          d.reset();
        }}
      />
      <DeleteButton
        label={profile.display_name ?? profile.profile_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteInferenceProfileConfig({
              profileId: profile.profile_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function ProfilesPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "profiles",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.inferenceProfiles.map((p) => ({
        id: p.profile_id,
        title: p.display_name ?? p.profile_id,
        meta: `${p.model_name} · ${deployment.inferenceBackends.find((backend) => backend.backendId === p.backend_id)?.name ?? p.backend_id}`,
      }))}
      createLabel="New profile"
      empty="No inference profiles."
      onCreate={async () => {
        const profile_id = newId("profile");
        const backend = deployment.inferenceBackends[0];
        if (!backend) throw new Error("Add a backend first");
        const model = backend.models[0];
        if (!model) throw new Error("Probe the backend and discover a model first");
        await shell.applyConfig((api) =>
          api.saveInferenceProfileConfig({
            document: {
              agent_did: deployment.agentDid,
              profile_id,
              display_name: "New profile",
              backend_id: backend.backendId,
              model_name: model,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "profiles",
          item: profile_id,
        });
      }}
      detail={(id) => {
        const profile = deployment.inferenceProfiles.find((p) => p.profile_id === id)!;
        return (
          <Editor
            key={profile.profile_id}
            shell={shell}
            deployment={deployment}
            profile={profile}
          />
        );
      }}
    />
  );
}
