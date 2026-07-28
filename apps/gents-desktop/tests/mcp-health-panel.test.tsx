import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  createDesktopApiAdapter,
  type MCPServiceHealthView,
  type McpServiceProbeResult,
} from "@source-inc/gents-desktop-client";
import { createMemoryTransport } from "@source-inc/gents-desktop-client/testing";
import {
  McpHealthPanel,
  McpHealthPanelView,
} from "@source-inc/gents-desktop-operations";

const mockedList = vi.fn<() => Promise<MCPServiceHealthView[]>>();
const mockedProbe = vi.fn<(serviceId: string) => Promise<McpServiceProbeResult>>();
const api = createDesktopApiAdapter(
  createMemoryTransport({
    handlers: {
      desktop_list_mcp_services_with_health: () => mockedList(),
      desktop_probe_mcp_service: (args) => {
        const { request } = args as { request: { serviceId: string } };
        return mockedProbe(request.serviceId);
      },
    },
  }),
);

function svc(
  overrides: Partial<MCPServiceHealthView> & { serviceId: string },
): MCPServiceHealthView {
  return {
    serviceId: overrides.serviceId,
    agentDid: overrides.agentDid ?? "did:test:agent-1",
    endpoint: overrides.endpoint ?? "100.69.4.79:9201/mcp",
    status: overrides.status ?? "healthy",
    failureCount: overrides.failureCount ?? 0,
    kMax: overrides.kMax ?? 3,
    backoffUntil: overrides.backoffUntil ?? null,
    lastProbeAt: overrides.lastProbeAt ?? new Date().toISOString(),
    lastSeen: overrides.lastSeen ?? new Date().toISOString(),
    lastErrorClass: overrides.lastErrorClass ?? null,
    lastErrorMessage: overrides.lastErrorMessage ?? null,
    updatedAt: overrides.updatedAt ?? new Date().toISOString(),
  };
}

describe("McpHealthPanelView", () => {
  it("renders service status rows and invokes the probe callback", () => {
    const onProbe = vi.fn();
    render(
      <McpHealthPanelView
        services={[
          svc({ serviceId: "ok-svc", status: "healthy" }),
          svc({
            serviceId: "evicted-svc",
            status: "evicted",
            failureCount: 3,
            backoffUntil: new Date(Date.now() + 30_000).toISOString(),
          }),
        ]}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={onProbe}
        onRefresh={vi.fn()}
      />,
    );

    expect(screen.getByTestId("mcp-health-status-ok-svc")).toHaveTextContent("healthy");
    expect(screen.getByTestId("mcp-health-status-evicted-svc")).toHaveTextContent(
      "evicted (backoff)",
    );

    fireEvent.click(screen.getByTestId("mcp-health-probe-ok-svc"));
    expect(onProbe).toHaveBeenCalledWith("ok-svc");
  });
});

describe("McpHealthPanel probe feedback", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedList.mockResolvedValue([svc({ serviceId: "ok-svc", status: "healthy" })]);
  });

  it("renders the live probe result after a successful probe", async () => {
    mockedProbe.mockResolvedValue({
      serviceId: "ok-svc",
      status: "healthy",
      latencyMs: 42,
      lastError: null,
    });

    render(<McpHealthPanel api={api} />);

    fireEvent.click(await screen.findByTestId("mcp-health-probe-ok-svc"));

    const result = await screen.findByTestId("mcp-health-probe-result-ok-svc");
    expect(result).toHaveTextContent("healthy · 42 ms");
    expect(mockedProbe).toHaveBeenCalledWith("ok-svc");
  });

  it("renders a per-row failure when the probe call itself fails", async () => {
    mockedProbe.mockRejectedValue(new Error("bridge unavailable"));

    render(<McpHealthPanel api={api} />);

    fireEvent.click(await screen.findByTestId("mcp-health-probe-ok-svc"));

    const result = await screen.findByTestId("mcp-health-probe-result-ok-svc");
    expect(result).toHaveTextContent("live probe failed: bridge unavailable");
  });
});
