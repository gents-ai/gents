import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  InferenceProfile,
  InferenceSampling,
  InferenceExecution,
} from "@source-inc/gents-desktop-client";
import { InferenceProfileConfigEditor } from "../src/components/config/InferenceProfileConfigPanel";

const profile: InferenceProfile = {
  agent_did: "owner",
  profile_id: " profile ",
  backend_id: "backend",
  model_name: "model",
  sampling_id: "sampling",
  execution_id: "execution",
  reasoning_effort: "high",
  tags: ["preserve"],
};
const sampling: InferenceSampling = {
  agent_did: "owner",
  sampling_id: "sampling",
  temperature: 0.7,
  top_p: 0.9,
  tags: ["sampling"],
};
const execution: InferenceExecution = {
  agent_did: "owner",
  execution_id: "execution",
  max_turns: 10,
  max_total_tokens: 1000,
  retry_policy_id: "retry",
  tags: ["execution"],
};
function props() {
  return {
    agentDid: "owner",
    profile,
    samplingConfigs: [sampling],
    executionConfigs: [execution],
    savedStatus: null,
    saving: false,
    onSaved: vi.fn(),
    onApplyConfigComponents: vi.fn().mockResolvedValue(undefined),
    onDeleteInferenceProfileConfig: vi.fn(),
    onDeleted: vi.fn(),
  };
}
describe("canonical profile editing", () => {
  it("preserves unedited canonical policy fields and applies all documents atomically", async () => {
    const handlers = props();
    render(<InferenceProfileConfigEditor {...handlers} />);
    fireEvent.change(screen.getByTestId("profile-temperature"), {
      target: { value: "1" },
    });
    fireEvent.change(screen.getByTestId("profile-model-name"), {
      target: { value: "selected" },
    });
    fireEvent.click(screen.getByTestId("profile-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    const { document } = handlers.onApplyConfigComponents.mock.calls[0][0];
    expect(document.agent_principal).toEqual({ agent_did: "owner" });
    expect(document.inference_profiles[0]).toMatchObject({
      ...profile,
      model_name: "selected",
    });
    expect(document.inference_sampling[0]).toMatchObject({
      ...sampling,
      temperature: 1,
    });
    expect(document.inference_execution[0]).toMatchObject(execution);
  });
  it("names new policy documents explicitly and keeps their creation with the profile update", async () => {
    const handlers = props();
    render(<InferenceProfileConfigEditor {...handlers} />);
    fireEvent.change(screen.getByTestId("profile-execution-id"), {
      target: { value: "new-execution" },
    });
    fireEvent.change(screen.getByTestId("profile-max-turns"), {
      target: { value: "1000" },
    });
    fireEvent.click(screen.getByTestId("profile-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    const { document } = handlers.onApplyConfigComponents.mock.calls[0][0];
    expect(document.inference_profiles[0].execution_id).toBe("new-execution");
    expect(document.inference_execution[0]).toMatchObject({
      agent_did: "owner",
      execution_id: "new-execution",
      max_turns: 1000,
    });
    expect(document.inference_execution[0]).not.toHaveProperty("retry_policy_id");
  });
  it("requires an explicit policy ID for configured execution values", async () => {
    const handlers = props();
    render(
      <InferenceProfileConfigEditor
        {...handlers}
        profile={{ ...profile, execution_id: null }}
      />,
    );
    fireEvent.change(screen.getByTestId("profile-max-turns"), {
      target: { value: "1000" },
    });
    fireEvent.click(screen.getByTestId("profile-save"));
    await screen.findByText(/Choose an execution document ID/);
    expect(handlers.onApplyConfigComponents).not.toHaveBeenCalled();
  });
});
