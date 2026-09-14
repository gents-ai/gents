import type {
  InferenceModelRecommendation,
  ReasoningEffort,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";

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
    maxOutputTokens: recommendation.maxOutputTokens?.recommended.toString() ?? "",
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
    if (!value.trim() || !Number.isFinite(number)) return `${label} is required`;
    if (integer && !Number.isInteger(number)) return `${label} must be a whole number`;
    if (number < control.min || (control.max != null && number > control.max))
      return `${label} must be between ${control.min} and ${control.max ?? "∞"}`;
    return null;
  };
  return (
    check("Context window", draft.contextWindow, recommendation.contextWindow, true) ??
    check(
      "Max output tokens",
      draft.maxOutputTokens,
      recommendation.maxOutputTokens,
      true,
    ) ??
    check("Temperature", draft.temperature, recommendation.temperature, false) ??
    check("Top-p", draft.topP, recommendation.topP, false) ??
    check("Concurrency", draft.maxConcurrent, recommendation.maxConcurrent, true)
  );
}

function NumericField({
  id,
  label,
  value,
  min,
  max,
  step,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  min: number;
  max: number | null;
  step?: number;
  onChange: (value: string) => void;
}) {
  return (
    <label className="grid gap-1" htmlFor={id}>
      <span className="text-xs text-muted-foreground">{label}</span>
      <Input
        id={id}
        type="number"
        value={value}
        min={min}
        max={max ?? undefined}
        step={step ?? 1}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}

export function InferenceModelControls({
  recommendation,
  value,
  onChange,
  expanded,
  onExpandedChange,
  includeConcurrency = true,
  alwaysExpanded = false,
}: {
  recommendation: InferenceModelRecommendation;
  value: InferenceSettingsDraft;
  onChange: (value: InferenceSettingsDraft) => void;
  expanded: boolean;
  onExpandedChange: (expanded: boolean) => void;
  includeConcurrency?: boolean;
  alwaysExpanded?: boolean;
}) {
  const set = <Key extends keyof InferenceSettingsDraft>(
    key: Key,
    next: InferenceSettingsDraft[Key],
  ) => onChange({ ...value, [key]: next });
  return (
    <div className="grid gap-3">
      {recommendation.temperature ? (
        <NumericField
          id="inference-temperature"
          label="Temperature"
          value={value.temperature}
          min={recommendation.temperature.min}
          max={recommendation.temperature.max}
          step={recommendation.temperature.step}
          onChange={(next) => set("temperature", next)}
        />
      ) : null}
      {recommendation.topP ? (
        <NumericField
          id="inference-top-p"
          label="Top-p"
          value={value.topP}
          min={recommendation.topP.min}
          max={recommendation.topP.max}
          step={recommendation.topP.step}
          onChange={(next) => set("topP", next)}
        />
      ) : null}
      {recommendation.reasoningEffort ? (
        <label className="grid gap-1" htmlFor="inference-reasoning">
          <span className="text-xs text-muted-foreground">Reasoning effort</span>
          <Select
            items={recommendation.reasoningEffort.choices.map((choice) => ({
              value: choice,
              label: choice,
            }))}
            value={value.reasoningEffort}
            onValueChange={(next) =>
              next && set("reasoningEffort", next as ReasoningEffort)
            }
          >
            <SelectTrigger id="inference-reasoning" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {recommendation.reasoningEffort.choices.map((choice) => (
                <SelectItem key={choice} value={choice}>
                  {choice}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
      ) : null}
      {!alwaysExpanded && (
        <Button
          type="button"
          variant="outline"
          className="justify-self-start"
          aria-expanded={expanded}
          onClick={() => onExpandedChange(!expanded)}
        >
          {expanded ? "Hide advanced settings" : "Advanced settings"}
        </Button>
      )}
      {expanded || alwaysExpanded ? (
        <div className="grid grid-cols-2 gap-3" data-testid="inference-custom-controls">
          {recommendation.contextWindow ? (
            <NumericField
              id="inference-context-window"
              label="Context window"
              value={value.contextWindow}
              min={recommendation.contextWindow.min}
              max={recommendation.contextWindow.max}
              onChange={(next) => set("contextWindow", next)}
            />
          ) : null}
          {recommendation.maxOutputTokens ? (
            <NumericField
              id="inference-max-output"
              label="Max output tokens"
              value={value.maxOutputTokens}
              min={recommendation.maxOutputTokens.min}
              max={recommendation.maxOutputTokens.max}
              onChange={(next) => set("maxOutputTokens", next)}
            />
          ) : null}
          {includeConcurrency ? (
            <NumericField
              id="inference-concurrency"
              label="Concurrent requests"
              value={value.maxConcurrent}
              min={recommendation.maxConcurrent.min}
              max={recommendation.maxConcurrent.max}
              onChange={(next) => set("maxConcurrent", next)}
            />
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
