import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createDesktopApiAdapter,
  type BackendHealth,
  type DesktopListSubagentTreeRequest,
  type SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import { createMemoryTransport } from "@source-inc/gents-desktop-client/testing";
import { BackendHealthPanel } from "@source-inc/gents-desktop-operations";
import { SubagentLineageView } from "@source-inc/gents-desktop-operations";

const mockedBackends = vi.fn<() => Promise<BackendHealth[]>>();
const mockedTree =
  vi.fn<(request: DesktopListSubagentTreeRequest) => Promise<SubagentTreeView>>();
const api = createDesktopApiAdapter(
  createMemoryTransport({
    handlers: {
      desktop_list_backends_with_health: () => mockedBackends(),
      desktop_list_subagent_tree: (args) => {
        const { request } = args as {
          request: DesktopListSubagentTreeRequest;
        };
        return mockedTree(request);
      },
    },
  }),
);

describe("ops panel liveness", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    mockedBackends.mockReset();
    mockedTree.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("backend health keeps polling instead of freezing at mount", async () => {
    mockedBackends.mockResolvedValue([]);
    render(<BackendHealthPanel api={api} />);
    await waitFor(() => expect(mockedBackends).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(10_000);
    expect(mockedBackends.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("advances backend relative ages on each poll", async () => {
    vi.setSystemTime(new Date("2026-07-17T12:00:00Z"));
    const backend: BackendHealth = {
      backendId: "backend-a",
      name: "Backend A",
      providerKind: "openai",
      endpoint: "http://localhost:1234/v1",
      enabled: true,
      probeStatus: "healthy",
      displayState: "available",
      lastProbe: "2026-07-17T12:00:00Z",
      maxConcurrent: 1,
      maxQueueDepth: 1,
      models: ["model-a"],
      recentCalls: [
        {
          callId: "call-a",
          callSeq: 1,
          callKind: "chat",
          callState: "completed",
          failureReason: null,
          queuedAt: "2026-07-17T12:00:00Z",
          startedAt: "2026-07-17T12:00:00Z",
          endedAt: "2026-07-17T12:00:00Z",
          queueDepthAtEnqueue: 0,
          promptTokens: 1,
          completionTokens: 1,
        },
      ],
    };
    mockedBackends.mockResolvedValue([backend]);

    render(<BackendHealthPanel api={api} />);
    await screen.findByText(/0s ago/);

    await vi.advanceTimersByTimeAsync(10_000);
    await waitFor(() => expect(screen.getByText(/10s ago/)).toBeInTheDocument());
  });

  it("lineage keeps polling while mounted and preserves collapse choices", async () => {
    mockedTree.mockResolvedValue({
      rootRequestId: "req_root",
      nodes: [{ requestId: "req_root" }],
      edges: [],
    } as never);

    render(
      <SubagentLineageView rootRequestId="req_root" agentDid="did:test:op" api={api} />,
    );
    await waitFor(() => expect(mockedTree).toHaveBeenCalledTimes(1));

    await vi.advanceTimersByTimeAsync(5_000);
    expect(mockedTree.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(screen.getAllByText(/req_root/).length).toBeGreaterThan(0);
  });

  it("lineage clears an initial error after a successful background retry", async () => {
    mockedTree
      .mockRejectedValueOnce(new Error("temporary bridge failure"))
      .mockResolvedValue({
        rootRequestId: "req_root",
        nodes: [{ requestId: "req_root" }],
        edges: [],
      } as never);

    render(
      <SubagentLineageView rootRequestId="req_root" agentDid="did:test:op" api={api} />,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "temporary bridge failure",
    );

    await vi.advanceTimersByTimeAsync(5_000);
    await waitFor(() =>
      expect(
        screen.getByRole("tree", { name: "Subagent lineage" }),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
