import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";

import { BackgroundedToolsPanel } from "../src/components/backgroundedTools";
import {
  setDesktopApiAdapterForTests,
  type DesktopApiAdapter,
} from "../src/lib/desktop-api";
import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
} from "../src/lib/types/operations";

function row(overrides: Partial<BackgroundedToolView> = {}): BackgroundedToolView {
  return {
    requestId: "req_a17",
    toolCallId: `tc_${Math.random().toString(36).slice(2, 8)}`,
    toolName: "grep",
    lifecycleState: "running",
    status: null,
    startedAt: new Date(Date.now() - 4_000).toISOString(),
    ageMs: 4_000,
    deadlineAt: new Date(Date.now() + 60_000).toISOString(),
    deadlineExpired: false,
    awaitMode: "background",
    cancelPolicy: "cascade",
    childRequestId: null,
    stuckSince: null,
    cancelPendingRemoteAck: false,
    nativeExecutor: null,
    ...overrides,
  };
}

function snapshot(toolCalls: BackgroundedToolView[]): DesktopOperationsSnapshot {
  return {
    fetchedAt: new Date().toISOString(),
    agentDid: null,
    liveness: {
      expiredProcessingCount: 0,
      requests: [],
      activeToolCalls: [],
      activeNativeExecutorsAvailable: true,
      activeNativeExecutors: [],
    },
    livenessUnavailableReason: null,
    backgroundedTools: toolCalls,
    stuckDiagnostics: [],
    lineage: null,
  };
}

function makeAdapter(
  fetchImpl: () => Promise<DesktopOperationsSnapshot>,
): DesktopApiAdapter {
  // Stub only the method this panel needs; other methods throw so they
  // fail loudly if accidentally called.
  return new Proxy({}, {
    get(_target, prop) {
      if (prop === "fetchOperationsSnapshot") {
        return (_request: unknown) => fetchImpl();
      }
      return () => {
        throw new Error(`DesktopApiAdapter.${String(prop)} not stubbed in this test`);
      };
    },
  }) as DesktopApiAdapter;
}

describe("BackgroundedToolsPanel", () => {
  afterEach(() => {
    setDesktopApiAdapterForTests(null);
    cleanup();
  });

  it("renders the empty state when the snapshot returns zero tools", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () => snapshot([])));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => {
      expect(screen.getByText(/no backgrounded tools/i)).toBeInTheDocument();
    });
  });

  it("renders the error state when the snapshot command rejects", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () => {
      throw new Error("desktop_operations_snapshot not implemented yet");
    }));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => {
      expect(screen.getByText(/snapshot bridge/i)).toBeInTheDocument();
    });
  });

  it("renders one row per backgrounded tool with derived status badges", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () =>
      snapshot([
        row({ toolCallId: "tc_running", toolName: "grep" }),
        row({ toolCallId: "tc_deadline", toolName: "fetch_remote", deadlineExpired: true }),
      ]),
    ));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    expect(screen.getByText("fetch_remote")).toBeInTheDocument();
    expect(screen.getByText(/deadline\+/i)).toBeInTheDocument();
  });

  it("marks a stuck row with the row-stuck class", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () =>
      snapshot([
        row({
          toolCallId: "tc_stuck",
          toolName: "index_repo",
          stuckSince: new Date(Date.now() - 12_000).toISOString(),
          cancelPendingRemoteAck: true,
        }),
      ]),
    ));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("index_repo")).toBeInTheDocument());
    const tr = screen.getByText("index_repo").closest("tr");
    expect(tr?.className).toContain("row-stuck");
  });

  it("hides healthy rows when 'show only stuck' toggle is on", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () =>
      snapshot([
        row({ toolCallId: "tc_healthy", toolName: "grep" }),
        row({
          toolCallId: "tc_stuck",
          toolName: "index_repo",
          stuckSince: new Date(Date.now() - 12_000).toISOString(),
        }),
      ]),
    ));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText(/show only stuck/i));
    expect(screen.queryByText("grep")).not.toBeInTheDocument();
    expect(screen.getByText("index_repo")).toBeInTheDocument();
  });

  it("filters by state chip (past deadline)", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () =>
      snapshot([
        row({ toolCallId: "tc_a", toolName: "grep" }),
        row({ toolCallId: "tc_b", toolName: "fetch_remote", deadlineExpired: true }),
      ]),
    ));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /past deadline/i }));
    expect(screen.queryByText("grep")).not.toBeInTheDocument();
    expect(screen.getByText("fetch_remote")).toBeInTheDocument();
  });

  it("sorts by age descending by default and toggles to ascending on header click", async () => {
    setDesktopApiAdapterForTests(makeAdapter(async () =>
      snapshot([
        row({ toolCallId: "tc_young", toolName: "grep_young", startedAt: new Date(Date.now() - 2_000).toISOString(), ageMs: 2_000 }),
        row({ toolCallId: "tc_old", toolName: "grep_old", startedAt: new Date(Date.now() - 200_000).toISOString(), ageMs: 200_000 }),
      ]),
    ));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep_young")).toBeInTheDocument());
    const rows = screen.getAllByRole("row").slice(1); // skip header
    expect(rows[0].textContent).toContain("grep_old");
    fireEvent.click(screen.getByRole("columnheader", { name: /age/i }));
    const rowsAsc = screen.getAllByRole("row").slice(1);
    expect(rowsAsc[0].textContent).toContain("grep_young");
  });
});
