import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { BackendConfigPanel } from "../src/components/config";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

const deployment = {
  deploymentId: "dep-1",
  agentDid: "did:test:operator",
  displayName: "test",
  defaultBehaviorId: "default",
  behaviors: [{ behaviorId: "default", displayName: "default" }],
  conversations: [],
  process: null,
  runtime: null,
  inbox: { hasUnread: false, count: 0 },
  inferenceBackends: [
    {
      backendId: "backend-a",
      name: "Backend A",
      providerKind: "openai",
      endpoint: "http://localhost:1234/v1",
      models: ["m-1"],
      enabled: true,
    },
  ],
} as unknown as DeploymentView;

describe("visible validation reasons", () => {
  it("explains an invalid numeric field instead of silently disabling Save", () => {
    render(
      <BackendConfigPanel
        deployment={deployment}
        selectedBackendId="backend-a"
        saving={false}
        savedStatus={null}
        onSelectBackend={vi.fn()}
        onCreateBackend={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSaveBackendConfig={vi.fn()}
      />,
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId("backend-max-concurrent"), {
      target: { value: "0" },
    });
    expect(screen.getByRole("alert")).toHaveTextContent("Whole number of 1 or more");

    fireEvent.change(screen.getByTestId("backend-max-concurrent"), {
      target: { value: "4" },
    });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
