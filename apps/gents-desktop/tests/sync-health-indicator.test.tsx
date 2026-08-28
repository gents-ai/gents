import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SyncHealthIndicator } from "../src/components/SyncHealthIndicator";
import type { SyncHealthView } from "@source-inc/gents-desktop-client";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    since: null,
    offlineSince: null,
    stalledSince: null,
    lastErrorClass: null,
    lastError: null,
    pairingRetryCount: 0,
    routeRetryCount: 0,
    connectedPeerCount: 1,
    ...overrides,
  };
}

describe("SyncHealthIndicator", () => {
  it("does not claim health before a known projection exists", () => {
    const { rerender } = render(<SyncHealthIndicator syncHealth={null} />);
    expect(screen.queryByTestId("sync-health-indicator")).not.toBeInTheDocument();

    rerender(<SyncHealthIndicator syncHealth={health({ state: "future-state" })} />);
    expect(screen.queryByTestId("sync-health-indicator")).not.toBeInTheDocument();
  });

  it("renders each product state and opens diagnostics", () => {
    const { rerender } = render(<SyncHealthIndicator syncHealth={health()} />);
    expect(screen.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "healthy",
    );
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent("Sync healthy");

    rerender(<SyncHealthIndicator syncHealth={health({ state: "syncing" })} />);
    expect(screen.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "syncing",
    );

    rerender(<SyncHealthIndicator syncHealth={health({ state: "stalled" })} />);
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent("Sync stalled");

    rerender(
      <SyncHealthIndicator
        syncHealth={health({
          state: "offline",
          offlineSince: "2020-01-01T00:00:00Z",
        })}
      />,
    );
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent(
      "Offline since",
    );

    rerender(
      <SyncHealthIndicator
        deployments={[
          {
            label: "Studio",
            agentDid: "did:test:agent",
            dialSucceeded: false,
            lastError: "unauthorized",
            pairing: [
              {
                collectionId: "AgentSession",
                pairingRetryCount: 1,
                lastRetryAt: null,
                lastRetryErrorClass: "RemoteUnauthorized",
                stuckSince: null,
              },
            ],
            routes: [],
          } as never,
        ]}
        syncHealth={health({
          state: "failed",
          lastErrorClass: "RemoteUnauthorized",
          lastError: "unauthorized",
        })}
      />,
    );
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent("Sync failed");
    fireEvent.click(screen.getByTestId("sync-health-summary"));
    expect(screen.getByTestId("sync-health-details")).toHaveTextContent(
      "RemoteUnauthorized",
    );
    expect(screen.getByTestId("sync-health-details")).toHaveTextContent("AgentSession");
    expect(screen.getByTestId("sync-health-details")).toHaveTextContent("unauthorized");
  });
});
