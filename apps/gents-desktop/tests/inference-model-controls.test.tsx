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

  it("keeps reasoning effort with the settings that can be changed later (#1618)", () => {
    const withEffort: InferenceModelRecommendation = {
      ...recommendation,
      reasoningEffort: { recommended: "medium", choices: ["low", "medium", "high"] },
    };
    const draft = recommendedInferenceSettings(withEffort);
    const view = render(
      <InferenceModelControls
        recommendation={withEffort}
        value={draft}
        onChange={vi.fn()}
        expanded={false}
        onExpandedChange={vi.fn()}
      />,
    );
    expect(screen.queryByRole("combobox", { name: "Reasoning effort" })).toBeNull();
    expect(screen.getByTestId("inference-settings-later")).toHaveTextContent(
      "Reasoning effort medium. You can change the model, reasoning effort and limits later",
    );
    view.rerender(
      <InferenceModelControls
        recommendation={withEffort}
        value={draft}
        onChange={vi.fn()}
        expanded
        onExpandedChange={vi.fn()}
      />,
    );
    expect(screen.getByRole("combobox", { name: "Reasoning effort" })).toBeVisible();
    expect(
      screen.getByText("Applies to new requests. You can change it at any time."),
    ).toBeVisible();
  });
});
