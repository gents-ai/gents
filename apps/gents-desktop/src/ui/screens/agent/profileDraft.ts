/* An inference profile as its editor drafts it: the profile with its
   sampling and execution documents as text, the numbers among them with
   their labels and bounds, and what makes a draft unsavable. */
import type { InferenceProfile } from "@source-inc/gents-desktop-client";

import type { NodeView } from "../../../hooks/fleetStore";
import {
  optionalInteger,
  optionalNumber,
  problemOf,
  str,
  type Problems,
} from "./draft";

export function profileDraftFrom(profile: InferenceProfile, deployment: NodeView) {
  const sampling = deployment.inferenceSampling.find(
    (row) => row.sampling_id === profile.sampling_id,
  );
  const execution = deployment.inferenceExecution.find(
    (row) => row.execution_id === profile.execution_id,
  );
  return {
    displayName: profile.display_name ?? "",
    description: profile.description ?? "",
    backendId: profile.backend_id,
    modelName: profile.model_name,
    reasoningEffort: profile.reasoning_effort ?? "",
    contextWindow: str(profile.context_window),
    maxOutputTokens: str(profile.max_output_tokens),
    samplingId: profile.sampling_id ?? "",
    temperature: str(sampling?.temperature),
    topP: str(sampling?.top_p),
    topK: str(sampling?.top_k),
    seed: str(sampling?.seed),
    minP: str(sampling?.min_p),
    frequencyPenalty: str(sampling?.frequency_penalty),
    presencePenalty: str(sampling?.presence_penalty),
    repetitionPenalty: str(sampling?.repetition_penalty),
    executionId: profile.execution_id ?? "",
    maxTurns: str(execution?.max_turns),
    maxTotalTokens: str(execution?.max_total_tokens),
    streamBatchMs: str(execution?.stream_batch_ms),
    streamLivenessSecs: str(execution?.stream_liveness_timeout_secs),
    providerIdleSecs: str(execution?.provider_idle_timeout_secs),
    deadlineSecs: str(execution?.deadline_duration_secs),
    retryPolicyId: execution?.retry_policy_id ?? "",
    tags: profile.tags ?? [],
  };
}

export type ProfileDraft = ReturnType<typeof profileDraftFrom>;

type NumberSpec = { label: string; integer: boolean; min?: number; max?: number };

/** Each number a profile draft holds, labelled as its field is. */
export const PROFILE_NUMBERS = {
  contextWindow: { label: "Context window", integer: true, min: 1 },
  maxOutputTokens: { label: "Max output tokens", integer: true, min: 1 },
  temperature: { label: "Temperature", integer: false, min: 0 },
  topP: { label: "Top P", integer: false, min: 0, max: 1 },
  topK: { label: "Top K", integer: true, min: 1 },
  seed: { label: "Seed", integer: true, min: 0 },
  minP: { label: "Min P", integer: false, min: 0, max: 1 },
  frequencyPenalty: { label: "Frequency penalty", integer: false, min: -2, max: 2 },
  presencePenalty: { label: "Presence penalty", integer: false, min: -2, max: 2 },
  repetitionPenalty: {
    label: "Repetition penalty",
    integer: false,
    min: Number.MIN_VALUE,
  },
  maxTurns: { label: "Max turns", integer: true, min: 1 },
  maxTotalTokens: { label: "Max total tokens", integer: true, min: 1 },
  streamBatchMs: { label: "Stream batch ms", integer: true, min: 1 },
  streamLivenessSecs: { label: "Execution lease seconds", integer: true, min: 1 },
  providerIdleSecs: { label: "Provider idle seconds", integer: true, min: 1 },
  deadlineSecs: { label: "Deadline seconds", integer: true, min: 1 },
} as const satisfies Partial<Record<keyof ProfileDraft, NumberSpec>>;

export type ProfileNumber = keyof typeof PROFILE_NUMBERS;

const EXECUTION_FIELDS = [
  "maxTurns",
  "maxTotalTokens",
  "streamBatchMs",
  "streamLivenessSecs",
  "providerIdleSecs",
  "deadlineSecs",
] as const satisfies readonly ProfileNumber[];

export type ProfileContext = {
  deployment: NodeView;
  /** a model that advertises only its maximum context has no model-aware
      control, but that maximum still bounds the profile's window */
  contextMax: number | undefined;
  /** the window and output limits are fields of their own, the model's
      recommendation having none for them */
  limitsShown: { contextWindow: boolean; maxOutputTokens: boolean };
};

/** A number of the draft; throws its problem. */
export function profileNumber(
  draft: ProfileDraft,
  field: ProfileNumber,
  { contextMax }: Pick<ProfileContext, "contextMax">,
): number | null {
  const spec: NumberSpec = PROFILE_NUMBERS[field];
  const bounds = {
    min: spec.min,
    max: field === "contextWindow" && contextMax !== undefined ? contextMax : spec.max,
  };
  return spec.integer
    ? optionalInteger(spec.label, draft[field], bounds)
    : optionalNumber(spec.label, draft[field], bounds);
}

/** Every number of the draft; throws the first problem. */
export function profileNumbers(
  draft: ProfileDraft,
  context: Pick<ProfileContext, "contextMax">,
): Record<ProfileNumber, number | null> {
  return Object.fromEntries(
    (Object.keys(PROFILE_NUMBERS) as ProfileNumber[]).map((field) => [
      field,
      profileNumber(draft, field, context),
    ]),
  ) as Record<ProfileNumber, number | null>;
}

/**
 * What is wrong with a profile draft, at the fields the editor draws. The
 * sampling values beyond temperature and top-p have no field here, and
 * the model-aware settings are checked against the recommendation when
 * saved, so neither is said at a field.
 */
export function profileProblems(
  draft: ProfileDraft,
  context: ProfileContext,
): Problems<ProfileDraft> {
  const { deployment, limitsShown } = context;
  const out: Problems<ProfileDraft> = {};
  if (!draft.backendId.trim()) out.backendId = "Backend is required";
  else if (!deployment.inferenceBackends.some((b) => b.backendId === draft.backendId))
    out.backendId = "Choose an existing backend";
  if (!draft.modelName.trim()) out.modelName = "Model is required";
  const number = (field: ProfileNumber) =>
    problemOf(() => profileNumber(draft, field, context));
  if (limitsShown.contextWindow) out.contextWindow = number("contextWindow");
  if (limitsShown.maxOutputTokens) out.maxOutputTokens = number("maxOutputTokens");
  for (const field of EXECUTION_FIELDS) out[field] = number(field);
  const executionValuesPresent =
    EXECUTION_FIELDS.some((field) => draft[field].trim()) || draft.retryPolicyId.trim();
  if (executionValuesPresent && !draft.executionId.trim())
    out.executionId = "Execution values require an execution document ID";
  if (!out.streamLivenessSecs && !out.deadlineSecs) {
    const lease = profileNumber(draft, "streamLivenessSecs", context);
    const deadline = profileNumber(draft, "deadlineSecs", context);
    if (lease != null && deadline != null && lease >= deadline)
      out.streamLivenessSecs = "Execution lease must be less than the deadline";
  }
  return out;
}
