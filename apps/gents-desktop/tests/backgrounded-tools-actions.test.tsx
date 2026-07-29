import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { BackgroundedToolsPanel } from "@source-inc/gents-desktop-operations";
import { useOperationsSnapshot } from "@source-inc/gents-desktop-operations";
import {
  OperationsRailProvider,
  useOperationsRail,
} from "@source-inc/gents-desktop-operations";
import type { DesktopOperationsSnapshot } from "@source-inc/gents-desktop-client";

const mockedSnapshot = vi.fn<typeof useOperationsSnapshot>();

const snapshot: DesktopOperationsSnapshot = {
  fetchedAt: new Date().toISOString(),
  backgroundedTools: [
    {
      requestId: "req_parent",
      toolCallId: "tc_1",
      toolName: "bash",
      ageMs: 12_000,
      deadlineExpired: false,
      cancelPendingRemoteAck: false,
      awaitMode: "background",
    },
  ],
  stuckDiagnostics: [],
};

function ActiveTabProbe() {
  const { activeTabId } = useOperationsRail();
  return <span data-testid="active-tab">{activeTabId}</span>;
}

const railTabs = [
  { id: "background-tools", label: "Background", render: () => null },
  { id: "lineage", label: "Lineage", render: () => null },
];

describe("BackgroundedToolsPanel row actions", () => {
  beforeEach(() => {
    mockedSnapshot.mockReturnValue({
      snapshot,
      error: null,
      isLoading: false,
      refresh: vi.fn(),
    });
  });

  it("Interrupt invokes onInterruptParent with the row's parent request id", () => {
    const onInterruptParent = vi.fn();
    render(
      <OperationsRailProvider tabs={railTabs}>
        <BackgroundedToolsPanel
          onOpenLineage={vi.fn()}
          onInterruptParent={onInterruptParent}
          useSnapshot={mockedSnapshot}
        />
      </OperationsRailProvider>,
    );

    fireEvent.click(screen.getByTestId("bg-tool-interrupt-tc_1"));
    expect(onInterruptParent).toHaveBeenCalledWith("req_parent");
  });

  it("Lineage invokes onOpenLineage and activates the lineage tab", () => {
    const onOpenLineage = vi.fn();
    render(
      <OperationsRailProvider tabs={railTabs}>
        <ActiveTabProbe />
        <BackgroundedToolsPanel
          onOpenLineage={onOpenLineage}
          onInterruptParent={vi.fn()}
          useSnapshot={mockedSnapshot}
        />
      </OperationsRailProvider>,
    );

    expect(screen.getByTestId("active-tab")).toHaveTextContent("background-tools");
    fireEvent.click(screen.getByTestId("bg-tool-lineage-tc_1"));
    expect(onOpenLineage).toHaveBeenCalledWith("req_parent");
    expect(screen.getByTestId("active-tab")).toHaveTextContent("lineage");
  });

  it("actions are disabled when rendered without handlers or a rail", () => {
    render(<BackgroundedToolsPanel useSnapshot={mockedSnapshot} />);

    expect(screen.getByTestId("bg-tool-lineage-tc_1")).toBeDisabled();
    expect(screen.getByTestId("bg-tool-interrupt-tc_1")).toBeDisabled();
  });
});
