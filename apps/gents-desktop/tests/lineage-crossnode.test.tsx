import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listSubagentTree } from "@source-inc/gents-desktop-client";
import { SubagentLineageView } from "@source-inc/gents-desktop-operations";

vi.mock("@source-inc/gents-desktop-client", async (orig) => ({
  ...(await orig()),
  listSubagentTree: vi.fn(),
}));
const mockedTree = vi.mocked(listSubagentTree);

describe("cross-node lineage", () => {
  beforeEach(() => mockedTree.mockReset());

  it("warns when a deployment could not be queried", async () => {
    mockedTree.mockResolvedValue({
      rootRequestId: "req_root",
      nodes: [{ requestId: "req_root" }],
      edges: [],
      truncated: false,
      partialErrors: ["Edge Rack: subagent tree level fetch query failed"],
    } as never);
    render(<SubagentLineageView rootRequestId="req_root" agentDid="did:test:op" />);

    await waitFor(() =>
      expect(screen.getByTestId("lineage-partial-errors")).toHaveTextContent(
        "Edge Rack",
      ),
    );
  });

  it("stays silent when every deployment answered", async () => {
    mockedTree.mockResolvedValue({
      rootRequestId: "req_root",
      nodes: [{ requestId: "req_root", resolvedVia: "Edge Rack" }],
      edges: [],
      truncated: false,
      partialErrors: [],
    } as never);
    render(<SubagentLineageView rootRequestId="req_root" agentDid="did:test:op" />);

    await waitFor(() => expect(mockedTree).toHaveBeenCalled());
    expect(screen.queryByTestId("lineage-partial-errors")).not.toBeInTheDocument();
  });
});
