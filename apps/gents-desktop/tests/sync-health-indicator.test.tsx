import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SyncHealthIndicator } from "../src/components/SyncHealthIndicator";
import type { SyncHealthView } from "@source-inc/gents-desktop-client";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    since: null,
    offlineSince: null,
    lastError: null,
    connectedPeerCount: 1,
    pendingDagCount: 0,
    persistedPendingDagCount: 0,
    pushRetryMarkerCount: 0,
    exhaustedFetchCount: 0,
    quarantinedDagCount: 0,
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
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent("Offline for");

    rerender(
      <SyncHealthIndicator
        syncHealth={health({
          state: "failed",
          lastError: "DefraDB quarantined a document DAG that could not be merged",
          quarantinedDagCount: 1,
        })}
      />,
    );
    expect(screen.getByTestId("sync-health-summary")).toHaveTextContent("Sync failed");
    fireEvent.click(screen.getByTestId("sync-health-summary"));
    expect(screen.getByTestId("sync-health-details")).toHaveTextContent(
      "Quarantined DAGs1",
    );
    expect(screen.getByTestId("sync-health-details")).toHaveTextContent("DefraDB");
  });
});
