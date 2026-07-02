import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../src/lib/desktop-api", () => ({
  listMcpServicesWithHealth: vi.fn(),
  probeMcpService: vi.fn(),
}));

import { listMcpServicesWithHealth, probeMcpService } from "../src/lib/desktop-api";
import { McpHealthPanel, McpHealthPanelView } from "../src/components/mcpHealth";
import type { MCPServiceHealthView } from "../src/lib/types";

const mockedList = vi.mocked(listMcpServicesWithHealth);
const mockedProbe = vi.mocked(probeMcpService);

function svc(
  overrides: Partial<MCPServiceHealthView> & { serviceId: string },
): MCPServiceHealthView {
  return {
    serviceId: overrides.serviceId,
    agentDid: overrides.agentDid ?? "did:defra:agent-1",
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

    render(<McpHealthPanel />);

    fireEvent.click(await screen.findByTestId("mcp-health-probe-ok-svc"));

    const result = await screen.findByTestId("mcp-health-probe-result-ok-svc");
    expect(result).toHaveTextContent("healthy · 42 ms");
    expect(mockedProbe).toHaveBeenCalledWith("ok-svc");
  });

  it("renders a per-row failure when the probe call itself fails", async () => {
    mockedProbe.mockRejectedValue(new Error("bridge unavailable"));

    render(<McpHealthPanel />);

    fireEvent.click(await screen.findByTestId("mcp-health-probe-ok-svc"));

    const result = await screen.findByTestId("mcp-health-probe-result-ok-svc");
    expect(result).toHaveTextContent("live probe failed: bridge unavailable");
  });
});
