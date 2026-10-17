import type { InferenceModelRecommendation, ReasoningEffort } from "./types.js";

export type InferenceSettingsDraft = {
  contextWindow: string;
  maxOutputTokens: string;
  temperature: string;
  topP: string;
  reasoningEffort: ReasoningEffort | "";
  maxConcurrent: string;
};

export function recommendedInferenceSettings(
  recommendation: InferenceModelRecommendation,
): InferenceSettingsDraft {
  return {
    contextWindow: recommendation.contextWindow?.recommended.toString() ?? "",
    maxOutputTokens:
      recommendation.maxOutputTokens?.recommended.toString() ?? "",
    temperature: recommendation.temperature?.recommended.toString() ?? "",
    topP: recommendation.topP?.recommended.toString() ?? "",
    reasoningEffort: recommendation.reasoningEffort?.recommended ?? "",
    maxConcurrent: recommendation.maxConcurrent.recommended.toString(),
  };
}

export function validateInferenceSettings(
  recommendation: InferenceModelRecommendation,
  draft: InferenceSettingsDraft,
) {
  if (
    recommendation.reasoningEffort &&
    !recommendation.reasoningEffort.choices.includes(
      draft.reasoningEffort as ReasoningEffort,
    )
  ) {
    return "Choose a supported reasoning effort";
  }
  const check = (
    label: string,
    value: string,
    control: { min: number; max: number | null } | null | undefined,
    integer: boolean,
  ) => {
    if (!control) return null;
    const number = Number(value);
    if (!value.trim() || !Number.isFinite(number))
      return `${label} is required`;
    if (integer && !Number.isInteger(number))
      return `${label} must be a whole number`;
    if (number < control.min || (control.max != null && number > control.max))
      return `${label} must be between ${control.min} and ${control.max ?? "∞"}`;
    return null;
  };
  return (
    check(
      "Context window",
      draft.contextWindow,
      recommendation.contextWindow,
      true,
    ) ??
    check(
      "Max output tokens",
      draft.maxOutputTokens,
      recommendation.maxOutputTokens,
      true,
    ) ??
    check(
      "Temperature",
      draft.temperature,
      recommendation.temperature,
      false,
    ) ??
    check("Top-p", draft.topP, recommendation.topP, false) ??
    check(
      "Concurrency",
      draft.maxConcurrent,
      recommendation.maxConcurrent,
      true,
    )
  );
}
