import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  InferenceModelRecommendation,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { ProfilesPanel } from "../src/ui/screens/agent/ProfilesPanel";
import { deployment as fixture } from "./config-panel-wiring/fixtures";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, reject, resolve };
}

function recommendation(context: number): InferenceModelRecommendation {
  return {
    defaultsVersion: "test",
    summary: "Model defaults",
    contextWindow: { recommended: context, min: 1, max: context },
    maxOutputTokens: { recommended: 1000, min: 1, max: 2000 },
    temperature: null,
    topP: null,
    reasoningEffort: null,
    maxConcurrent: { recommended: 1, min: 1, max: null },
  };
}

function shellWith(api: Record<string, ReturnType<typeof vi.fn>>) {
  return {
    api: api as unknown as DesktopApiAdapter,
    applyConfig: (operation: (adapter: DesktopApiAdapter) => Promise<unknown>) =>
      operation(api as unknown as DesktopApiAdapter),
  } as unknown as Shell;
}

const settleLookup = () =>
  act(() => new Promise((resolve) => setTimeout(resolve, 180)));

describe("profile model recommendation ownership", () => {
  it("discards an out-of-order recommendation after the selected model changes", async () => {
    const first = deferred<InferenceModelRecommendation>();
    const second = deferred<InferenceModelRecommendation>();
    const api = {
      getInferenceBackendRecommendation: vi
        .fn()
        .mockReturnValueOnce(first.promise)
        .mockReturnValueOnce(second.promise),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    const shell = shellWith(api);
    const deploymentA = { ...fixture };
    const deploymentB = {
      ...fixture,
      inferenceProfiles: fixture.inferenceProfiles.map((profile) =>
        profile.profile_id === "profile-a"
          ? {
              ...profile,
              backend_id: "backend-b",
              model_name: "model-b",
              context_window: 222,
            }
          : profile,
      ),
    };
    const view = render(
      <ProfilesPanel shell={shell} deployment={deploymentA} item="profile-a" />,
    );
    await settleLookup();
    view.rerender(
      <ProfilesPanel shell={shell} deployment={deploymentB} item="profile-a" />,
    );
    await settleLookup();
    await act(async () => second.resolve(recommendation(222)));
    expect(await screen.findByLabelText("Context window")).toHaveValue(222);

    await act(async () => first.resolve(recommendation(111)));
    expect(screen.getByLabelText("Context window")).toHaveValue(222);
  });

  it("clears stale controls and reports a failed replacement lookup", async () => {
    const api = {
      getInferenceBackendRecommendation: vi
        .fn()
        .mockResolvedValueOnce(recommendation(111))
        .mockRejectedValueOnce(new Error("catalog unavailable")),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    const shell = shellWith(api);
    const view = render(
      <ProfilesPanel shell={shell} deployment={fixture} item="profile-a" />,
    );
    expect(await screen.findByLabelText("Context window")).toBeVisible();
    const changed = {
      ...fixture,
      inferenceProfiles: fixture.inferenceProfiles.map((profile) =>
        profile.profile_id === "profile-a"
          ? { ...profile, backend_id: "backend-b", model_name: "model-b" }
          : profile,
      ),
    };
    view.rerender(
      <ProfilesPanel shell={shell} deployment={changed} item="profile-a" />,
    );
    await settleLookup();

    expect(screen.queryByLabelText("Context window")).not.toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("catalog unavailable");
  });

  it("rejects settings outside the current model recommendation before writing", async () => {
    const api = {
      applyConfigComponents: vi.fn().mockResolvedValue({}),
      getInferenceBackendRecommendation: vi.fn().mockResolvedValue(recommendation(100)),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    render(
      <ProfilesPanel shell={shellWith(api)} deployment={fixture} item="profile-a" />,
    );
    await waitFor(() =>
      expect(api.getInferenceBackendRecommendation).toHaveBeenCalled(),
    );
    await screen.findByLabelText("Context window");
    const user = userEvent.setup();
    const name = screen.getByLabelText("Display name");
    await user.clear(name);
    await user.type(name, "Invalid profile");
    await user.click(screen.getByRole("button", { name: "Save changes" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Context window must be between 1 and 100",
    );
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("renders the reset draft instead of retaining an independent guided value", async () => {
    const api = {
      applyConfigComponents: vi.fn().mockResolvedValue({}),
      getInferenceBackendRecommendation: vi
        .fn()
        .mockResolvedValue(recommendation(200_000)),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    render(
      <ProfilesPanel shell={shellWith(api)} deployment={fixture} item="profile-a" />,
    );
    const context = await screen.findByLabelText("Context window");
    const user = userEvent.setup();
    fireEvent.change(context, { target: { value: "777" } });
    expect(context).toHaveValue(777);
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByLabelText("Context window")).toHaveValue(131072);
    expect(screen.getByLabelText("Max output tokens")).toHaveValue(1000);
    const name = screen.getByLabelText("Display name");
    await user.clear(name);
    await user.type(name, "Renamed");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.applyConfigComponents).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({
          inference_profiles: [
            expect.objectContaining({
              context_window: 131072,
              max_output_tokens: null,
            }),
          ],
        }),
      }),
    );
  });

  it("preserves a current-model edit made while a passive refresh is pending", async () => {
    const pending = deferred<InferenceModelRecommendation>();
    const api = {
      getInferenceBackendRecommendation: vi
        .fn()
        .mockResolvedValueOnce(recommendation(200_000))
        .mockReturnValueOnce(pending.promise),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    const shell = shellWith(api);
    const view = render(
      <ProfilesPanel shell={shell} deployment={fixture} item="profile-a" />,
    );
    const context = await screen.findByLabelText("Context window");
    fireEvent.change(context, { target: { value: "777" } });
    view.rerender(
      <ProfilesPanel
        shell={shell}
        deployment={{
          ...fixture,
          inferenceBackends: fixture.inferenceBackends.map((backend) =>
            backend.backendId === "backend-a"
              ? { ...backend, endpoint: `${backend.endpoint}/refreshed` }
              : backend,
          ),
        }}
        item="profile-a"
      />,
    );
    await settleLookup();
    await act(async () => pending.resolve(recommendation(300_000)));

    expect(screen.getByLabelText("Context window")).toHaveValue(777);
  });

  it("stays pristine on open and accepts an authoritative profile refresh", async () => {
    const api = {
      getInferenceBackendRecommendation: vi
        .fn()
        .mockResolvedValue(recommendation(300_000)),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    const shell = shellWith(api);
    const view = render(
      <ProfilesPanel shell={shell} deployment={fixture} item="profile-a" />,
    );
    expect(await screen.findByLabelText("Context window")).toHaveValue(131072);
    expect(screen.getByLabelText("Max output tokens")).toHaveValue(1000);
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();

    view.rerender(
      <ProfilesPanel
        shell={shell}
        deployment={{
          ...fixture,
          inferenceProfiles: fixture.inferenceProfiles.map((profile) =>
            profile.profile_id === "profile-a"
              ? { ...profile, context_window: 222222 }
              : profile,
          ),
        }}
        item="profile-a"
      />,
    );
    expect(await screen.findByLabelText("Context window")).toHaveValue(222222);
    expect(screen.getByRole("button", { name: "Save changes" })).toBeDisabled();
  });

  it("materializes a sampling ID when an advanced-only value is present", async () => {
    const api = {
      applyConfigComponents: vi.fn().mockResolvedValue({}),
      getInferenceBackendRecommendation: vi
        .fn()
        .mockResolvedValue(recommendation(300_000)),
      getInferenceSetupCatalog: vi.fn().mockResolvedValue({ executionDefaults: {} }),
    };
    const advancedOnly = {
      ...fixture,
      inferenceSampling: [
        {
          agent_did: fixture.agentDid,
          sampling_id: null,
          top_k: 42,
        } as never,
      ],
    };
    render(
      <ProfilesPanel
        shell={shellWith(api)}
        deployment={advancedOnly}
        item="profile-a"
      />,
    );
    await screen.findByLabelText("Context window");
    const user = userEvent.setup();
    const name = screen.getByLabelText("Display name");
    await user.clear(name);
    await user.type(name, "Advanced profile");
    await user.click(screen.getByRole("button", { name: "Save changes" }));

    expect(api.applyConfigComponents).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({
          inference_profiles: [
            expect.objectContaining({ sampling_id: "profile-a-sampling" }),
          ],
          inference_sampling: [
            expect.objectContaining({
              sampling_id: "profile-a-sampling",
              top_k: 42,
            }),
          ],
        }),
      }),
    );
  });
});
