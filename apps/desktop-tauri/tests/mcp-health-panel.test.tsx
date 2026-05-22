import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { McpHealthPanelView } from "../src/components/mcpHealth";
import type { MCPServiceHealthView } from "../src/lib/types";

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
