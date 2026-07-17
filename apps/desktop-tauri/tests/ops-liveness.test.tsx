import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../src/lib/desktop-api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/lib/desktop-api")>();
  return {
    ...original,
    listBackendsWithHealth: vi.fn(),
    listSubagentTree: vi.fn(),
  };
});

import { listBackendsWithHealth, listSubagentTree } from "../src/lib/desktop-api";
import { BackendHealthPanel } from "../src/components/backendHealth";
import { SubagentLineageView } from "../src/components/subagentLineage";

const mockedBackends = vi.mocked(listBackendsWithHealth);
const mockedTree = vi.mocked(listSubagentTree);

describe("ops panel liveness", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("backend health keeps polling instead of freezing at mount", async () => {
    mockedBackends.mockResolvedValue([]);
    render(<BackendHealthPanel />);
    await waitFor(() => expect(mockedBackends).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(10_000);
    expect(mockedBackends.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("lineage keeps polling while mounted and preserves collapse choices", async () => {
    mockedTree.mockResolvedValue({
      rootRequestId: "req_root",
      nodes: [{ requestId: "req_root" }],
      edges: [],
    } as never);

    render(<SubagentLineageView rootRequestId="req_root" agentDid="did:test:op" />);
    await waitFor(() => expect(mockedTree).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(5_000);
    expect(mockedTree.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText(/req_root/).length).toBeGreaterThan(0);
  });
});
