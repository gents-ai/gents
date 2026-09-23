import { useEffect, useRef, useState } from "react";
import { dependentsWarning } from "./dependents";
import type {
  BackendProviderKind,
  DeploymentView,
  InferenceExecution,
  InferenceModelRecommendation,
  InferenceProfile,
  InferenceSampling,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { href, navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  RefRow,
  TagsRow,
  TextRow,
} from "./editors";
import { ProfileSheet } from "./ProfileSheet";
import { InferencePanel } from "./InferencePanel";
import { BackendSheet } from "./BackendSheet";
import { Plus } from "lucide-react";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
import type { ListRow } from "./ListDetail";
import { SetupScreen } from "../setup/SetupScreen";
import { newId, optionalInteger, optionalNumber, str, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";
import {
  InferenceModelControls,
  recommendedInferenceSettings,
  validateInferenceSettings,
  type InferenceSettingsDraft,
} from "../inference/InferenceModelControls";

/* behaviors named in Used by before the rest are counted */
const USERS_SHOWN = 3;

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

/* the profile a draft starts from: the first backend that advertises a
   model, or the one asked for, and its first model */
export function newProfileDocument(
  deployment: DeploymentView,
  backendId?: string,
): InferenceProfile {
  const backend =
    deployment.inferenceBackends.find((b) => b.backendId === backendId) ??
    deployment.inferenceBackends.find((b) => b.models.length > 0) ??
    deployment.inferenceBackends[0];
  return {
    agent_did: deployment.agentDid,
    profile_id: newId("profile"),
    display_name: "",
    backend_id: backend?.backendId ?? "",
    model_name: backend?.models[0] ?? "",
  };
}

export function ProfileEditor({
  shell,
  deployment,
  profile,
  embedded = false,
  draft: draftMode,
}: {
  shell: Shell;
  deployment: DeploymentView;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
  profile: InferenceProfile;
  /* a new profile that exists only here until Create */
  draft?: { onSaved: (profileId: string) => void; onCancel: () => void };
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
    tags: profile.tags ?? [],
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
  const d = useDraft(
    saved,
    async (next) => {
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
      const maxOutputTokens = optionalInteger(
        "Max output tokens",
        next.maxOutputTokens,
        {
          min: 1,
        },
      );
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
        tags: next.tags.length ? next.tags : null,
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
    },
    { isNew: draftMode !== undefined },
  );
  const [executionDefaults, setExecutionDefaults] = useState<
    Record<string, number | null | undefined>
  >({});
  useEffect(() => {
    let canceled = false;
    if (shell.api.getInferenceSetupCatalog)
      void shell.api
        .getInferenceSetupCatalog()
        .then((catalog) => {
          if (!canceled) setExecutionDefaults(catalog.executionDefaults ?? {});
        })
        .catch(() => {});
    return () => {
      canceled = true;
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
    let canceled = false;
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
          if (canceled) return;
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
          if (canceled) return;
          setRecommendationError(
            `Model-aware settings unavailable: ${error instanceof Error ? error.message : String(error)}`,
          );
        });
    }, 150);
    return () => {
      canceled = true;
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
  /* the backend's full editor, open beside the model */
  const [besideBackend, setBesideBackend] = useState<string | null>(null);
  /* New backend… from the Backend field: the onboarding's provider step, in place */
  const [addingBackend, setAddingBackend] = useState<
    ((id: string | null) => void) | null
  >(null);
  if (addingBackend) {
    const before = new Set(deployment.inferenceBackends.map((b) => b.backendId));
    return (
      <SetupScreen
        shell={shell}
        initialStep="inference"
        purpose="add-backend"
        agentDid={deployment.agentDid}
        onCancel={() => {
          addingBackend(null);
          setAddingBackend(null);
        }}
        onDone={(snapshot) => {
          const added = (snapshot.client?.deployments ?? [])
            .find((x) => x.agentDid === deployment.agentDid)
            ?.inferenceBackends.find((b) => !before.has(b.backendId));
          addingBackend(added?.backendId ?? null);
          setAddingBackend(null);
        }}
      />
    );
  }
  const usedBy = deployment.behaviors.filter(
    (b) => b.inferenceProfileId === profile.profile_id,
  );
  return (
    <>
      <BackendSheet
        shell={shell}
        deployment={deployment}
        backendId={besideBackend}
        onClose={() => setBesideBackend(null)}
      />
      {!embedded && (
        <header className="mb-6">
          <h2 className="font-heading text-lg text-heading">
            {profile.display_name ?? profile.profile_id}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {modelSentence(deployment, profile)}
          </p>
        </header>
      )}
      <Group title="Model">
        {!draftMode && (
          <FactRow label="Profile ID" mono>
            {profile.profile_id}
          </FactRow>
        )}
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
        <RefRow
          id={id("backend")}
          label="Backend"
          description="The provider and endpoint the model is served from."
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
          createLabel="New backend…"
          onCreate={() =>
            new Promise<string | null>((resolve) => {
              setAddingBackend(() => resolve);
            })
          }
          onOpen={(backendId) => setBesideBackend(backendId)}
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
        <FactRow label="Used by">
          {usedBy.length ? (
            <span className="flex flex-wrap justify-end gap-x-3 gap-y-1">
              {usedBy.slice(0, USERS_SHOWN).map((b) => (
                <a
                  key={b.behaviorId}
                  href={href({ ...base, section: "behaviors", item: b.behaviorId })}
                  className="max-w-48 truncate underline-offset-2 hover:text-foreground hover:underline"
                >
                  {b.displayName}
                </a>
              ))}
              {usedBy.length > USERS_SHOWN && (
                <span>and {usedBy.length - USERS_SHOWN} more</span>
              )}
            </span>
          ) : (
            "No behavior yet. Pick it under a behavior’s Model."
          )}
        </FactRow>
      </Group>
      {recommendation && guided ? (
        <Group title="Model-aware defaults">
          <div className="px-5 py-4">
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
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        saveLabel={draftMode ? "Create" : undefined}
        onSave={() =>
          draftMode
            ? void d.save().then((ok) => ok && draftMode.onSaved(profile.profile_id))
            : d.save()
        }
        onCancel={() => {
          if (draftMode) {
            draftMode.onCancel();
            return;
          }
          setEditedExecution(new Set());
          setEditedModelFields(new Set());
          d.reset();
        }}
      />
      {!embedded && !draftMode && (
        <DeleteButton
          label={profile.display_name ?? profile.profile_id}
          warning={dependentsWarning(deployment, "profile", profile.profile_id)}
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
      )}
    </>
  );
}

/* "claude-sonnet-5 · via Anthropic · 2 behaviors" */
export function modelSentence(deployment: DeploymentView, p: InferenceProfile) {
  const backend = deployment.inferenceBackends.find(
    (b) => b.backendId === p.backend_id,
  );
  const users = deployment.behaviors.filter(
    (b) => b.inferenceProfileId === p.profile_id,
  ).length;
  return [
    p.model_name,
    `via ${backend?.name ?? p.backend_id}`,
    users ? `${users} ${users === 1 ? "behavior" : "behaviors"}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

/* why a model cannot serve right now */
function modelProblem(deployment: DeploymentView, p: InferenceProfile): string | null {
  const backend = deployment.inferenceBackends.find(
    (b) => b.backendId === p.backend_id,
  );
  if (!backend) return "Backend is missing";
  if (backend.enabled === false) return "Backend is disabled";
  if (backend.authKind === "api_key" && !backend.apiKeyConfigured)
    return "Backend has no API key";
  return null;
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
  /* Add profile under a backend: the dialog opens on it */
  const [creating, setCreating] = useState<string | null>(null);
  const modelRows = (b: InferenceBackendView): ListRow[] => [
    ...deployment.inferenceProfiles
      .filter((p) => p.backend_id === b.backendId)
      .map((p) => {
        const problem = modelProblem(deployment, p);
        return {
          id: p.profile_id,
          href: href({ ...base, item: p.profile_id }),
          title: p.display_name ?? p.profile_id,
          meta: (() => {
            const users = deployment.behaviors.filter(
              (b) => b.inferenceProfileId === p.profile_id,
            ).length;
            return `${p.model_name}${users ? ` · ${users} ${users === 1 ? "behavior" : "behaviors"}` : ""}`;
          })(),
          badge: problem ?? undefined,
          badgeTone: "bad" as const,
          tags: p.tags,
          trailing: (
            <RowMenu
              name={p.display_name ?? p.profile_id}
              base={base}
              id={p.profile_id}
              onDuplicate={async () => {
                const profile_id = newId("profile");
                await shell.applyConfig((api) =>
                  api.saveInferenceProfileConfig({
                    document: {
                      ...p,
                      profile_id,
                      display_name: `${p.display_name ?? p.profile_id} copy`,
                    },
                  }),
                );
                return profile_id;
              }}
              onDelete={() =>
                shell.applyConfig((api) =>
                  api.deleteInferenceProfileConfig({
                    profileId: p.profile_id,
                    agentDid: deployment.agentDid,
                  }),
                )
              }
              warning={(() => {
                const n = deployment.behaviors.filter(
                  (x) => x.inferenceProfileId === p.profile_id,
                ).length;
                return n
                  ? `${n} ${n === 1 ? "behavior loses" : "behaviors lose"} its profile.`
                  : undefined;
              })()}
            />
          ),
        };
      }),
    {
      id: `add:${b.backendId}`,
      title: "Add profile",
      meta: b.models.length
        ? `${b.models.length} advertised`
        : "probe the backend for models first",
      icon: <Plus className="size-3.5 text-muted-foreground" />,
      onOpen: () => setCreating(b.backendId),
    },
  ];
  if (item) {
    const profile = deployment.inferenceProfiles.find((p) => p.profile_id === item);
    if (profile)
      return (
        <ListDetail
          base={base}
          item={item}
          rows={[
            {
              id: profile.profile_id,
              title: profile.display_name ?? profile.profile_id,
            },
          ]}
          createLabel=""
          empty=""
          detail={() => (
            <ProfileEditor
              key={profile.profile_id}
              shell={shell}
              deployment={deployment}
              profile={profile}
            />
          )}
        />
      );
  }
  return (
    <>
      <ProfileSheet
        shell={shell}
        deployment={deployment}
        open={creating !== null}
        backendId={creating ?? undefined}
        onClose={(profileId) => {
          setCreating(null);
          if (profileId) navigate({ ...base, item: profileId });
        }}
      />
      <InferencePanel shell={shell} deployment={deployment} under={modelRows} />
    </>
  );
}
