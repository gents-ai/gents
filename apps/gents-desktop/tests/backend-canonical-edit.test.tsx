import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
import { BackendConfigEditor } from "../src/components/config/BackendConfigPanel";

const backend: InferenceBackendView = {
  backendId: " backend ",
  name: "Backend",
  providerKind: "OpenAiCompatible",
  openaiWireApi: "responses",
  endpoint: "http://localhost:8000/v1",
  apiKeyConfigured: true,
  apiKeyEnvVar: null,
  maxConcurrent: 1,
  maxQueueDepth: 10,
  enabled: true,
  models: ["advertised"],
  probeStatus: null,
};
function props() {
  return {
    agentDid: "did:test:owner",
    backend,
    savedStatus: null,
    saving: false,
    onSaved: vi.fn(),
    onSaveBackendConfig: vi.fn().mockResolvedValue(undefined),
    onPatchConfigComponents: vi.fn().mockResolvedValue(undefined),
    onDeleteBackendConfig: vi.fn(),
    onDeleted: vi.fn(),
  };
}
describe("canonical backend editing", () => {
  it("patches only the edited fields, preserves redacted auth, and accepts zero queue capacity", async () => {
    const handlers = props();
    const { rerender } = render(<BackendConfigEditor {...handlers} />);
    fireEvent.change(screen.getByTestId("backend-name"), {
      target: { value: "Edited" },
    });
    fireEvent.change(screen.getByTestId("backend-max-queue-depth"), {
      target: { value: "0" },
    });
    rerender(
      <BackendConfigEditor
        {...handlers}
        backend={{ ...backend, endpoint: "http://new-observation:8000/v1" }}
      />,
    );
    expect(screen.getByTestId("backend-models")).toHaveAttribute("readonly");
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(handlers.onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: "did:test:owner",
        patches: [
          {
            collection: "InferenceBackend",
            id: " backend ",
            changes: { name: "Edited", max_queue_depth: 0 },
          },
        ],
      }),
    );
    expect(handlers.onSaveBackendConfig).not.toHaveBeenCalled();
  });
  it("uses an explicit canonical auth replacement to clear stored credentials", async () => {
    const handlers = props();
    render(<BackendConfigEditor {...handlers} />);
    fireEvent.click(screen.getByTestId("backend-clear-api-key"));
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(handlers.onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: "did:test:owner",
        patches: [
          {
            collection: "InferenceBackend",
            id: " backend ",
            changes: { auth: { kind: "unauthenticated" } },
          },
        ],
      }),
    );
  });
  it("creates a canonical document without an invented catalog", async () => {
    const handlers = props();
    render(<BackendConfigEditor {...handlers} backend={null} />);
    for (const [field, value] of [
      ["backend-id", "new"],
      ["backend-name", "New"],
      ["backend-endpoint", "http://localhost:8000/v1"],
    ]) {
      fireEvent.change(screen.getByTestId(field), { target: { value } });
    }
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(handlers.onSaveBackendConfig).toHaveBeenCalledWith({
        document: {
          agent_did: "did:test:owner",
          backend_id: "new",
          name: "New",
          provider_kind: "OpenAiCompatible",
          endpoint: "http://localhost:8000/v1",
          auth: { kind: "unauthenticated" },
          max_concurrent: null,
          max_queue_depth: null,
          enabled: true,
        },
      }),
    );
    expect(handlers.onPatchConfigComponents).not.toHaveBeenCalled();
  });
});
