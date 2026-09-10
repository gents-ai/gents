import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import { InferenceSetupWizard } from "../../../packages/gents-desktop-fleet/src/InferenceSetupWizard";
import { deployment } from "./config-panel-wiring/fixtures";

function renderWizard(
  overrides: Partial<Parameters<typeof InferenceSetupWizard>[0]> = {},
) {
  const props = {
    deployment: {
      ...deployment,
      inferenceProfiles: [
        {
          agent_did: deployment.agentDid,
          profile_id: "profile-a",
          backend_id: "backend-a",
          model_name: "initial",
        },
      ],
    },
    onClose: vi.fn(),
    onPatchConfigComponents: vi.fn(async () => undefined),
    onProbeInferenceEndpoint: vi.fn(async () => ({ reachable: false, models: [] })),
    onCodexLogin: vi.fn(async () => ({
      docId: "doc-1",
      credentialId: "chatgpt-codex:did:key:z6MkAgent",
      agentDid: "did:key:z6MkAgent",
      provider: "chatgpt-codex",
      accountId: "acct-1",
      chatgptPlanType: "plus",
      isFedramp: false,
      accessTokenExpiresAt: "2026-08-01T00:00:00Z",
      enabled: true,
    })),
    ...overrides,
  };
  render(<InferenceSetupWizard {...props} />);
  return props;
}

describe("InferenceSetupWizard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("updates OpenAI connection and selected profile model atomically", async () => {
    const props = renderWizard();

    fireEvent.click(screen.getByTestId("inference-option-openai"));
    fireEvent.change(screen.getByTestId("inference-openai-key"), {
      target: { value: "sk-test" },
    });
    fireEvent.change(screen.getByTestId("inference-openai-model"), {
      target: { value: "gpt-5.4-mini" },
    });
    fireEvent.click(screen.getByTestId("inference-openai-save"));

    await waitFor(() => {
      expect(props.onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: "InferenceBackend",
            id: "backend-a",
            changes: expect.objectContaining({
              provider_kind: "OpenAiCompatible",
              openai_wire_api: "responses",
              endpoint: "https://api.openai.com/v1",
              auth: { kind: "api_key", key: "sk-test" },
              enabled: true,
            }),
          },
          {
            collection: "InferenceProfile",
            id: "profile-a",
            changes: { model_name: "gpt-5.4-mini" },
          },
        ],
      });
    });
    expect(props.onPatchConfigComponents).toHaveBeenCalledTimes(1);
  });

  it("writes a chat-completions backend for a local server", async () => {
    const props = renderWizard();

    fireEvent.click(screen.getByTestId("inference-option-local"));
    fireEvent.change(screen.getByTestId("inference-local-url"), {
      target: { value: "http://127.0.0.1:11434/v1" },
    });
    fireEvent.change(screen.getByTestId("inference-local-model"), {
      target: { value: "llama-x" },
    });
    fireEvent.click(screen.getByTestId("inference-local-save"));

    await waitFor(() => {
      expect(props.onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: "InferenceBackend",
            id: "backend-a",
            changes: expect.objectContaining({
              provider_kind: "OpenAiCompatible",
              openai_wire_api: "chat_completions",
              endpoint: "http://127.0.0.1:11434/v1",
              auth: { kind: "unauthenticated" },
            }),
          },
          {
            collection: "InferenceProfile",
            id: "profile-a",
            changes: { model_name: "llama-x" },
          },
        ],
      });
    });
  });

  it("signs in with ChatGPT before writing the Codex backend", async () => {
    const props = renderWizard();

    fireEvent.click(screen.getByTestId("inference-option-codex"));
    fireEvent.click(screen.getByTestId("inference-codex-signin"));

    await waitFor(() => {
      expect(props.onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: "InferenceBackend",
            id: "backend-a",
            changes: expect.objectContaining({
              provider_kind: "ChatGptCodex",
              endpoint: "https://chatgpt.com/backend-api/codex",
              auth: { kind: "principal_o_auth" },
            }),
          },
          {
            collection: "InferenceProfile",
            id: "profile-a",
            changes: { model_name: "gpt-5.5" },
          },
        ],
      });
    });
    expect(props.onCodexLogin).toHaveBeenCalledWith("did:key:z6MkAgent");
    // Login must land before the backend flips to Codex, or the agent would
    // point at a Codex backend with no credential.
    const loginOrder = props.onCodexLogin.mock.invocationCallOrder[0];
    const saveOrder = props.onPatchConfigComponents.mock.invocationCallOrder[0];
    expect(loginOrder).toBeLessThan(saveOrder);
  });
});
