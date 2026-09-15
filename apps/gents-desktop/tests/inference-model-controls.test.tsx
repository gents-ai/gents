import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { InferenceModelRecommendation } from "@source-inc/gents-desktop-client";
import {
  InferenceModelControls,
  recommendedInferenceSettings,
  validateInferenceSettings,
} from "../src/ui/screens/inference/InferenceModelControls";

const recommendation: InferenceModelRecommendation = {
  defaultsVersion: "fixture",
  summary: "",
  contextWindow: { recommended: 272000, min: 1, max: 872000 },
  maxOutputTokens: null,
  temperature: null,
  topP: null,
  reasoningEffort: null,
  maxConcurrent: { recommended: 8, min: 1, max: null },
};

describe("model context controls", () => {
  it("starts at the default and permits overrides only through the advertised maximum", () => {
    const draft = recommendedInferenceSettings(recommendation);
    expect(draft.contextWindow).toBe("272000");
    for (const contextWindow of ["272000", "500000", "872000"]) {
      expect(
        validateInferenceSettings(recommendation, { ...draft, contextWindow }),
      ).toBeNull();
    }
    for (const contextWindow of ["872001", "0", "-1", "2.5", "", "NaN"]) {
      expect(
        validateInferenceSettings(recommendation, { ...draft, contextWindow }),
      ).not.toBeNull();
    }
  });

  it.each([false, true])(
    "shares default/max controls with expanded profile mode=%s",
    (alwaysExpanded) => {
      const onChange = vi.fn();
      const draft = recommendedInferenceSettings(recommendation);
      render(
        <InferenceModelControls
          recommendation={recommendation}
          value={draft}
          onChange={onChange}
          expanded={!alwaysExpanded}
          alwaysExpanded={alwaysExpanded}
          onExpandedChange={vi.fn()}
        />,
      );
      const input = screen.getByLabelText("Context window", { exact: false });
      expect(input).toHaveValue(272000);
      expect(input).toHaveAttribute("max", "872000");
      expect(screen.getByText("Default 272,000 · Maximum 872,000")).toBeInTheDocument();
      fireEvent.change(input, { target: { value: "872000" } });
      expect(onChange).toHaveBeenCalledWith({ ...draft, contextWindow: "872000" });
    },
  );

  it("retains the normal single-bound behavior when no larger maximum is supplied", () => {
    const singleBound = {
      ...recommendation,
      contextWindow: { recommended: 272000, min: 1, max: 272000 },
    };
    expect(
      validateInferenceSettings(singleBound, {
        ...recommendedInferenceSettings(singleBound),
        contextWindow: "272001",
      }),
    ).not.toBeNull();
  });
});
