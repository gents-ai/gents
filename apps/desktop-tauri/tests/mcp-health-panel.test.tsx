import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";

import { McpHealthPanelView } from "../src/components/mcpHealth";
import type { MCPServiceHealthView } from "../src/lib/types";

function svc(
  overrides: Partial<MCPServiceHealthView> & { serviceId: string },
): MCPServiceHealthView {
  // Use "in" rather than `??` so an explicit `null` override survives — the
  // unknown-status case is tested by passing `status: null`.
  const pick = <K extends keyof MCPServiceHealthView>(
    key: K,
    fallback: MCPServiceHealthView[K],
  ): MCPServiceHealthView[K] => (key in overrides ? overrides[key]! : fallback);
  return {
    serviceId: overrides.serviceId,
    agentDid: pick("agentDid", "did:defra:agent-1"),
    endpoint: pick("endpoint", "100.69.4.79:9201/mcp"),
    status: pick("status", "healthy"),
    failureCount: pick("failureCount", 0),
    kMax: pick("kMax", 3),
    backoffUntil: pick("backoffUntil", null),
    lastProbeAt: pick("lastProbeAt", new Date().toISOString()),
    lastSeen: pick("lastSeen", new Date().toISOString()),
    lastErrorClass: pick("lastErrorClass", null),
    lastErrorMessage: pick("lastErrorMessage", null),
    updatedAt: pick("updatedAt", new Date().toISOString()),
  };
}

describe("McpHealthPanelView", () => {
  it("renders distinct labels for healthy / degraded / evicted / reconnecting / stuck / unknown", () => {
    const services: MCPServiceHealthView[] = [
      svc({ serviceId: "ok-svc", status: "healthy", failureCount: 0 }),
      svc({
        serviceId: "degraded-svc",
        status: "degraded",
        failureCount: 1,
        lastErrorClass: "stream_closed",
        lastErrorMessage: "list_tools: stream closed by peer",
      }),
      svc({
        serviceId: "evicted-svc",
        status: "evicted",
        failureCount: 3,
        backoffUntil: new Date(Date.now() + 30_000).toISOString(),
        lastErrorClass: "connection_refused",
        lastErrorMessage: "tcp connect: connection refused",
      }),
      svc({
        serviceId: "reconnecting-svc",
        status: "reconnecting",
        failureCount: 3,
        backoffUntil: null,
      }),
      // failure_count >= 2*K triggers the derived "stuck" badge even though
      // the persisted status is still `reconnecting`.
      svc({
        serviceId: "stuck-svc",
        status: "reconnecting",
        failureCount: 9,
        kMax: 3,
        lastSeen: new Date(Date.now() - 30 * 60_000).toISOString(),
      }),
      svc({ serviceId: "unknown-svc", status: null }),
    ];

    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={new Date().toISOString()}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );

    const expect_status = (id: string, label: string) => {
      const cell = screen.getByTestId(`mcp-health-status-${id}`);
      expect(cell).toHaveTextContent(label);
    };
    expect_status("ok-svc", "healthy");
    expect_status("degraded-svc", "degraded");
    expect_status("evicted-svc", "evicted (backoff)");
    expect_status("reconnecting-svc", "reconnecting");
    expect_status("stuck-svc", "stuck");
    expect_status("unknown-svc", "unknown");
  });

  it("accepts the legacy 'stale' status string and renders it as degraded", () => {
    // Back-compat with any rows persisted before the Lean-vocab alignment.
    const services = [svc({ serviceId: "legacy-stale", status: "stale" })];
    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    const cell = screen.getByTestId("mcp-health-status-legacy-stale");
    expect(cell).toHaveTextContent("degraded");
  });

  it("filter chip 'unhealthy' hides the healthy row but keeps degraded/evicted/reconnecting", () => {
    const services = [
      svc({ serviceId: "ok-svc", status: "healthy" }),
      svc({ serviceId: "degraded-svc", status: "degraded", failureCount: 1 }),
      svc({ serviceId: "evicted-svc", status: "evicted", failureCount: 3 }),
    ];
    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /Unhealthy/i }));
    expect(screen.queryByTestId("mcp-health-row-ok-svc")).toBeNull();
    expect(screen.getByTestId("mcp-health-row-degraded-svc")).toBeInTheDocument();
    expect(screen.getByTestId("mcp-health-row-evicted-svc")).toBeInTheDocument();
  });

  it("K=1 row renders the legacy single-fail badge, K=3 row renders K=N · n/N", () => {
    const services = [
      svc({ serviceId: "legacy-k1", status: "reconnecting", failureCount: 1, kMax: 1 }),
      svc({ serviceId: "kn-evicted", status: "evicted", failureCount: 3, kMax: 3 }),
    ];
    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    const k1Row = screen.getByTestId("mcp-health-row-legacy-k1");
    expect(within(k1Row).getByText("K=1")).toBeInTheDocument();
    expect(within(k1Row).getByText("single-fail → evict")).toBeInTheDocument();

    const knRow = screen.getByTestId("mcp-health-row-kn-evicted");
    expect(within(knRow).getByText("K=3 · 3/3")).toBeInTheDocument();
  });

  it("invokes onProbe with the service id when the Probe button is clicked", () => {
    const services = [svc({ serviceId: "ok-svc", status: "healthy" })];
    const onProbe = vi.fn();
    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={onProbe}
        onRefresh={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("mcp-health-probe-ok-svc"));
    expect(onProbe).toHaveBeenCalledWith("ok-svc");
  });

  it("renders the empty state when no services are registered", () => {
    render(
      <McpHealthPanelView
        services={[]}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByText(/No MCP services registered/i)).toBeInTheDocument();
  });

  it("expands a row to the detail panel on click and collapses on Escape", () => {
    const services = [svc({ serviceId: "ok-svc", status: "healthy" })];
    render(
      <McpHealthPanelView
        services={services}
        loading={false}
        error={null}
        lastFetchedAt={null}
        probingServiceId={null}
        onProbe={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );
    const row = screen.getByTestId("mcp-health-row-ok-svc");
    expect(row).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(row);
    expect(screen.getByTestId("mcp-health-row-ok-svc")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    fireEvent.keyDown(row, { key: "Escape" });
    expect(screen.getByTestId("mcp-health-row-ok-svc")).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });
});
