import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The Codex step attaches a Tauri event listener for the auth-url fallback;
// stub it so the component mounts and runs outside the native shell.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import { InferenceSetupWizard } from "@source-inc/gents-desktop-fleet/local-runtime";
import { deployment } from "./config-panel-wiring/fixtures";

function renderWizard(
  overrides: Partial<Parameters<typeof InferenceSetupWizard>[0]> = {},
) {
  const props = {
    deployment,
    onClose: vi.fn(),
    onSaveBackendConfig: vi.fn(async () => undefined),
    onSaveBehaviorConfig: vi.fn(async () => undefined),
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

  it("writes an OpenAI Responses backend and rebinds the default behavior", async () => {
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
      expect(props.onSaveBackendConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          backendId: "backend-a",
          providerKind: "openai",
          openaiWireApi: "responses",
          endpoint: "https://api.openai.com/v1",
          apiKey: "sk-test",
          models: ["gpt-5.4-mini"],
          enabled: true,
        }),
      );
    });
    // The behavior is re-saved so it re-derives its model from the new backend.
    expect(props.onSaveBehaviorConfig).toHaveBeenCalledWith(
      expect.objectContaining({ behaviorId: "default", backendId: "backend-a" }),
    );
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
      expect(props.onSaveBackendConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          providerKind: "openai",
          openaiWireApi: "chat_completions",
          endpoint: "http://127.0.0.1:11434/v1",
          models: ["llama-x"],
          clearApiKey: true,
        }),
      );
    });
  });

  it("signs in with ChatGPT before writing the Codex backend", async () => {
    const props = renderWizard();

    fireEvent.click(screen.getByTestId("inference-option-codex"));
    fireEvent.click(screen.getByTestId("inference-codex-signin"));

    await waitFor(() => {
      expect(props.onSaveBackendConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          providerKind: "ChatGptCodex",
          endpoint: "https://chatgpt.com/backend-api/codex",
          models: ["gpt-5.5"],
        }),
      );
    });
    expect(props.onCodexLogin).toHaveBeenCalledWith("did:key:z6MkAgent");
    // Login must land before the backend flips to Codex, or the agent would
    // point at a Codex backend with no credential.
    const loginOrder = props.onCodexLogin.mock.invocationCallOrder[0];
    const saveOrder = props.onSaveBackendConfig.mock.invocationCallOrder[0];
    expect(loginOrder).toBeLessThan(saveOrder);
  });
});
