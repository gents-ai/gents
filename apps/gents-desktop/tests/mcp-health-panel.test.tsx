import { act, fireEvent, render, screen, within } from "@testing-library/react";
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
const mockedProbe =
  vi.fn<(nodeDid: string, serviceId: string) => Promise<McpServiceProbeResult>>();
const api = createDesktopApiAdapter(
  createMemoryTransport({
    handlers: {
      desktop_list_mcp_services_with_health: () => mockedList(),
      desktop_probe_mcp_service: (args) => {
        const { request } = args as { request: { nodeDid: string; serviceId: string } };
        return mockedProbe(request.nodeDid, request.serviceId);
      },
    },
  }),
);

// Mirrors ToolServiceHealthState::project (crates/gents-protocol/src/tool_service_health.rs)
// so fixtures don't drift from the server's classification.
function displayStateFor(status: string): "healthy" | "stale" | "unreachable" {
  switch (status) {
    case "healthy":
      return "healthy";
    case "degraded":
      return "stale";
    case "evicted":
    case "reconnecting":
      return "unreachable";
    default:
      return "unreachable";
  }
}

function svc(
  overrides: Partial<MCPServiceHealthView> & { serviceId: string },
): MCPServiceHealthView {
  const status = overrides.status ?? "healthy";
  return {
    serviceId: overrides.serviceId,
    nodeDid: overrides.nodeDid ?? "did:test:node-1",
    endpoint: overrides.endpoint ?? "100.69.4.79:9201/mcp",
    status,
    displayState: overrides.displayState ?? displayStateFor(status),
    failureCount: overrides.failureCount ?? 0,
    toolCount: overrides.toolCount ?? null,
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
        probingServiceIds={[]}
        onProbe={onProbe}
        onRefresh={vi.fn()}
      />,
    );

    expect(screen.getByTestId("mcp-health-status-ok-svc")).toHaveTextContent("healthy");
    expect(screen.getByTestId("mcp-health-status-evicted-svc")).toHaveTextContent(
      "unreachable",
    );

    fireEvent.click(screen.getByTestId("mcp-health-probe-ok-svc"));
    expect(onProbe).toHaveBeenCalledWith(JSON.stringify(["did:test:node-1", "ok-svc"]));
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
    expect(mockedProbe).toHaveBeenCalledWith("did:test:node-1", "ok-svc");
  });

  it("keeps identical service IDs on different nodes scoped through probe feedback", async () => {
    mockedList.mockResolvedValue([
      svc({ nodeDid: "did:test:first", serviceId: "shared" }),
      svc({ nodeDid: "did:test:second", serviceId: "shared" }),
    ]);
    let finish: (result: McpServiceProbeResult) => void = () => {};
    mockedProbe.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<McpHealthPanel api={api} />);
    const rows = await screen.findAllByTestId("mcp-health-row-shared");
    const first = within(rows[0]!);
    const second = within(rows[1]!);
    fireEvent.click(second.getByRole("button", { name: "Probe shared" }));
    expect(mockedProbe).toHaveBeenCalledExactlyOnceWith("did:test:second", "shared");
    expect(second.getByRole("button", { name: "Probe shared" })).toHaveAttribute(
      "aria-busy",
      "true",
    );
    expect(first.getByRole("button", { name: "Probe shared" })).toHaveAttribute(
      "aria-busy",
      "false",
    );
    await act(async () =>
      finish({
        serviceId: "shared",
        status: "healthy",
        latencyMs: 73,
        lastError: null,
      }),
    );
    expect(
      await second.findByTestId("mcp-health-probe-result-shared"),
    ).toHaveTextContent("healthy · 73 ms");
    expect(first.queryByTestId("mcp-health-probe-result-shared")).toBeNull();
    expect(second.getByRole("button", { name: "Probe shared" })).toHaveAttribute(
      "aria-busy",
      "false",
    );
  });

  it("keeps different nodes pending independently and admits one probe per key", async () => {
    mockedList.mockResolvedValue([
      svc({ nodeDid: "did:test:first", serviceId: "shared" }),
      svc({ nodeDid: "did:test:second", serviceId: "shared" }),
    ]);
    const finishes = new Map<string, (result: McpServiceProbeResult) => void>();
    mockedProbe.mockImplementation(
      (nodeDid) => new Promise((resolve) => finishes.set(nodeDid, resolve)),
    );
    render(<McpHealthPanel api={api} />);
    const rows = await screen.findAllByTestId("mcp-health-row-shared");
    const first = within(rows[0]!);
    const second = within(rows[1]!);
    const firstButton = first.getByRole("button", { name: "Probe shared" });
    const secondButton = second.getByRole("button", { name: "Probe shared" });
    act(() => {
      fireEvent.click(firstButton);
      fireEvent.click(firstButton);
      fireEvent.click(secondButton);
    });
    expect(mockedProbe.mock.calls).toEqual([
      ["did:test:first", "shared"],
      ["did:test:second", "shared"],
    ]);
    expect(firstButton).toHaveAttribute("aria-busy", "true");
    expect(secondButton).toHaveAttribute("aria-busy", "true");
    await act(async () =>
      finishes.get("did:test:second")!({
        serviceId: "shared",
        status: "healthy",
        latencyMs: 22,
        lastError: null,
      }),
    );
    expect(firstButton).toHaveAttribute("aria-busy", "true");
    expect(secondButton).toHaveAttribute("aria-busy", "false");
    expect(second.getByTestId("mcp-health-probe-result-shared")).toHaveTextContent(
      "healthy · 22 ms",
    );
    expect(first.queryByTestId("mcp-health-probe-result-shared")).toBeNull();
    await act(async () =>
      finishes.get("did:test:first")!({
        serviceId: "shared",
        status: "healthy",
        latencyMs: 11,
        lastError: null,
      }),
    );
    expect(firstButton).toHaveAttribute("aria-busy", "false");
    expect(first.getByTestId("mcp-health-probe-result-shared")).toHaveTextContent(
      "healthy · 11 ms",
    );
    expect(second.getByTestId("mcp-health-probe-result-shared")).toHaveTextContent(
      "healthy · 22 ms",
    );
  });

  it("renders a per-row failure when the probe call itself fails", async () => {
    mockedProbe.mockRejectedValue(new Error("bridge unavailable"));

    render(<McpHealthPanel api={api} />);

    fireEvent.click(await screen.findByTestId("mcp-health-probe-ok-svc"));

    const result = await screen.findByTestId("mcp-health-probe-result-ok-svc");
    expect(result).toHaveTextContent("live probe failed: bridge unavailable");
  });
});
