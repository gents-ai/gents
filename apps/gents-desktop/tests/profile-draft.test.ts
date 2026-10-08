import { describe, expect, it } from "vitest";

import type { InferenceProfile } from "@source-inc/gents-desktop-client";

import {
  profileDraftFrom,
  profileProblems,
  type ProfileContext,
} from "../src/ui/screens/agent/profileDraft";
import { deployment } from "./config-panel-wiring/fixtures";

const profile = deployment.inferenceProfiles[0] as InferenceProfile;
const draft = (over: Record<string, string> = {}) => ({
  ...profileDraftFrom(profile, deployment),
  ...over,
});
const context: ProfileContext = {
  deployment,
  contextMax: undefined,
  limitsShown: { contextWindow: false, maxOutputTokens: false },
};

describe("a profile draft's problems", () => {
  it("says each execution problem at its own field", () => {
    const problems = profileProblems(
      draft({
        executionId: "",
        maxTurns: "two",
        streamLivenessSecs: "",
        deadlineSecs: "",
      }),
      context,
    );
    expect(problems.maxTurns).toBe("Max turns must be a whole number");
    expect(problems.executionId).toBe(
      "Execution values require an execution document ID",
    );
  });

  it("holds a lease no shorter than the deadline at the lease", () => {
    const problems = profileProblems(
      draft({ executionId: "x", streamLivenessSecs: "60", deadlineSecs: "30" }),
      context,
    );
    expect(problems.streamLivenessSecs).toBe(
      "Execution lease must be less than the deadline",
    );
  });

  it("checks the window and output limits only where they are fields", () => {
    const bad = draft({ contextWindow: "0", maxOutputTokens: "lots" });
    expect(profileProblems(bad, context).contextWindow).toBeUndefined();
    const shown = profileProblems(bad, {
      ...context,
      limitsShown: { contextWindow: true, maxOutputTokens: true },
    });
    expect(shown.contextWindow).toBe("Context window must be 1 or more");
    expect(shown.maxOutputTokens).toBe("Max output tokens must be a whole number");
  });

  it("needs a backend the node lists and a model", () => {
    const problems = profileProblems(
      draft({ backendId: "gone", modelName: " " }),
      context,
    );
    expect(problems.backendId).toBe("Choose an existing backend");
    expect(problems.modelName).toBe("Model is required");
  });
});
