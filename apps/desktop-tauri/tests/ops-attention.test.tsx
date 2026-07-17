import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../src/components/backgroundedTools/useOperationsSnapshot", () => ({
  useOperationsSnapshot: vi.fn(),
}));

import { BackgroundedToolsPanel } from "../src/components/backgroundedTools";
import { useOperationsSnapshot } from "../src/components/backgroundedTools/useOperationsSnapshot";
import { OperationsRail, OperationsRailProvider } from "../src/components/operations";
import type { DesktopOperationsSnapshot } from "../src/lib/types/operations";

const mockedSnapshot = vi.mocked(useOperationsSnapshot);

const snapshot: DesktopOperationsSnapshot = {
  fetchedAt: new Date().toISOString(),
  backgroundedTools: [],
  stuckDiagnostics: [
    {
      requestId: "req_0123456789abcdef",
      severity: "warning",
      reason: "stuckTool",
      toolName: "bash",
      toolCallId: "tc1",
    },
    {
      requestId: "req_critical",
      severity: "critical",
      reason: "expiredProcessing",
    },
  ],
};

describe("stuck-work attention", () => {
  beforeEach(() => {
    mockedSnapshot.mockReturnValue({
      snapshot,
      error: null,
      isLoading: false,
      refresh: vi.fn(),
    });
  });

  it("renders diagnostics in operator language inside the panel", () => {
    render(<BackgroundedToolsPanel agentDid="did:key:z6MkSelected" />);
    expect(mockedSnapshot).toHaveBeenCalledWith({
      agentDid: "did:key:z6MkSelected",
      rootRequestId: null,
    });
    const strip = screen.getByTestId("stuck-diagnostics");
    expect(strip).toHaveTextContent(
      "bash on req_0123456789… has stopped making progress",
    );
    expect(strip).toHaveTextContent("Request req_critical ran past its deadline");
  });

  it("badges the collapsed rail handle with the attention count", () => {
    render(
      <OperationsRailProvider
        tabs={[{ id: "background-tools", label: "Background", render: () => null }]}
      >
        <OperationsRail open={false} attentionCount={2} />
      </OperationsRailProvider>,
    );
    expect(screen.getByTestId("ops-attention")).toHaveTextContent("2");
    expect(
      screen.getByRole("button", { name: /2 items need attention/i }),
    ).toBeInTheDocument();
  });

  it("keeps the quiet handle when nothing needs attention", () => {
    render(
      <OperationsRailProvider
        tabs={[{ id: "background-tools", label: "Background", render: () => null }]}
      >
        <OperationsRail open={false} attentionCount={0} />
      </OperationsRailProvider>,
    );
    expect(screen.queryByTestId("ops-attention")).not.toBeInTheDocument();
  });
});
