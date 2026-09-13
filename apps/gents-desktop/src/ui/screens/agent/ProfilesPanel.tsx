import type {
  DeploymentView,
  InferenceExecution,
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
  const d = useDraft(saved, async (next) => {
    if (!next.backendId.trim()) throw new Error("Backend is required");
    if (!deployment.inferenceBackends.some((b) => b.backendId === next.backendId))
      throw new Error("Choose an existing backend");
    if (!next.modelName.trim()) throw new Error("Model is required");
    const samplingValuesPresent = [
      next.temperature,
      next.topP,
      next.topK,
      next.seed,
      next.minP,
      next.frequencyPenalty,
      next.presencePenalty,
      next.repetitionPenalty,
    ].some((value) => value.trim());
    if (samplingValuesPresent && !next.samplingId.trim())
      throw new Error("Sampling values require a sampling document ID");
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
    const temperature = optionalNumber("Temperature", next.temperature, { min: 0 });
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
      (row) => row.sampling_id === next.samplingId.trim(),
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
      sampling_id: next.samplingId.trim() || null,
      execution_id: next.executionId.trim() || null,
      tags: fromLinesOrNull(next.tags),
    };
    const nextSampling: InferenceSampling | null = next.samplingId.trim()
      ? {
          ...sampling,
          agent_did: deployment.agentDid,
          sampling_id: next.samplingId.trim(),
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
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <ChoiceRow
          id={id("backend")}
          label="Backend"
          value={d.draft.backendId}
          onChange={(v) => d.choose("backendId", v)}
          items={deployment.inferenceBackends.map((b) => ({
            value: b.backendId,
            label: b.name ?? b.backendId,
          }))}
        />
        <TextRow
          id={id("model")}
          label="Model"
          value={d.draft.modelName}
          onChange={(v) => d.set("modelName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("reasoning")}
          label="Reasoning effort"
          value={d.draft.reasoningEffort}
          onChange={(v) =>
            d.choose(
              "reasoningEffort",
              v as NonNullable<InferenceProfile["reasoning_effort"]> | "",
            )
          }
          items={[
            "none",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "ultra",
          ].map((value) => ({ value, label: value }))}
          none="Provider default"
        />
        <NumberRow
          id={id("context")}
          label="Context window"
          description="Positive whole number, or blank for model capability."
          value={d.draft.contextWindow}
          onChange={(v) => d.set("contextWindow", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("maxout")}
          label="Max output tokens"
          value={d.draft.maxOutputTokens}
          onChange={(v) => d.set("maxOutputTokens", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
      </Group>
      <Group title="Sampling">
        <TextRow
          id={id("sampling")}
          label="Sampling document ID"
          description="Reuse an existing ID or enter a new one for these settings."
          value={d.draft.samplingId}
          onChange={(v) => d.set("samplingId", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <NumberRow
          id={id("temperature")}
          label="Temperature"
          value={d.draft.temperature}
          onChange={(v) => d.set("temperature", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("top-p")}
          label="Top P"
          description="Between 0 and 1."
          value={d.draft.topP}
          onChange={(v) => d.set("topP", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("top-k")}
          label="Top K"
          description="Positive whole number."
          value={d.draft.topK}
          onChange={(v) => d.set("topK", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("seed")}
          label="Seed"
          description="Non-negative whole number."
          value={d.draft.seed}
          onChange={(v) => d.set("seed", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("min-p")}
          label="Min P"
          description="Between 0 and 1."
          value={d.draft.minP}
          onChange={(v) => d.set("minP", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("frequency-penalty")}
          label="Frequency penalty"
          description="Between -2 and 2."
          value={d.draft.frequencyPenalty}
          onChange={(v) => d.set("frequencyPenalty", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("presence-penalty")}
          label="Presence penalty"
          description="Between -2 and 2."
          value={d.draft.presencePenalty}
          onChange={(v) => d.set("presencePenalty", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("repetition-penalty")}
          label="Repetition penalty"
          description="Positive number."
          value={d.draft.repetitionPenalty}
          onChange={(v) => d.set("repetitionPenalty", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
      </Group>
      <Group title="Execution">
        <TextRow
          id={id("execution")}
          label="Execution document ID"
          description="Reuse an existing ID or enter a new one for these limits."
          value={d.draft.executionId}
          onChange={(v) => d.set("executionId", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <NumberRow
          id={id("max-turns")}
          label="Max turns"
          value={d.draft.maxTurns}
          onChange={(v) => d.set("maxTurns", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("max-total")}
          label="Max total tokens"
          value={d.draft.maxTotalTokens}
          onChange={(v) => d.set("maxTotalTokens", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("batch")}
          label="Stream batch ms"
          value={d.draft.streamBatchMs}
          onChange={(v) => d.set("streamBatchMs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("liveness")}
          label="Stream liveness seconds"
          value={d.draft.streamLivenessSecs}
          onChange={(v) => d.set("streamLivenessSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("deadline")}
          label="Deadline seconds"
          value={d.draft.deadlineSecs}
          onChange={(v) => d.set("deadlineSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("retry")}
          label="Retry policy ID"
          value={d.draft.retryPolicyId}
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
        onCancel={d.reset}
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
        meta: p.model_name,
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
