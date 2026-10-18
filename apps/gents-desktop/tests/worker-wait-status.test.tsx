import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  BackgroundedToolView,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, testApp, withApp } from "./app-fixture";
import { WorkerStep } from "../src/ui/screens/WorkerStep";
import { NO_WORKERS } from "../src/ui/screens/workers";

const AGENT = "did:key:parent";

const processTool = (
  toolCallId: string,
  statusKind = "running",
): RenderedToolCallView =>
  ({
    itemKey: `item-${toolCallId}`,
    toolName: "spawn_process",
    toolCallId,
    statusKind,
    requestId: "req-1",
    awaitMode: "background",
    presentation: {
      kind: "process",
      action: "spawn",
      target: "bash",
    },
  }) as unknown as RenderedToolCallView;

const bgView = (over: Partial<BackgroundedToolView>): BackgroundedToolView =>
  ({
    requestId: "req-1",
    toolCallId: "call-1",
    toolName: "spawn_process",
    lifecycleState: "running",
    status: null,
    startedAt: "2026-05-20T12:00:00Z",
    ageMs: null,
    deadlineAt: null,
    deadlineExpired: false,
    awaitMode: "background",
    stuckSince: null,
    nativeExecutor: null,
    waitStatus: {
      lifecycle: "running",
      lifecycleStateRaw: "running",
      startedAt: "2026-05-20T12:00:00Z",
      completedAt: null,
      runningElapsedSeconds: 42,
      terminalElapsedSeconds: null,
    },
    ...over,
  }) as unknown as BackgroundedToolView;

function fixtureApp(backgroundedTools: BackgroundedToolView[]): DesktopApp {
  return testApp({
    api: {
      sessionProvenance: vi.fn().mockResolvedValue({
        sessionId: "parent-session",
        startedBy: null,
        started: [],
        sent: [],
        received: [],
        senders: [],
        calls: [],
      }),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools }),
    },
    deployments: [
      node({
        nodeDid: AGENT,
        sessions: [{ nodeDid: AGENT, sessionId: "s", requesterDid: null }],
      }),
    ],
    session: {
      sessionId: "parent-session",
      timelineItems: [],
    } as never,
    selection: { nodeDid: AGENT, sessionId: "parent-session" },
  });
}

describe("WorkerStep wait-status projection consumption", () => {
  it("renders running age from the shared projection", async () => {
    const view = bgView({ toolCallId: "call-1" });
    render(
      <WorkerStep
        tool={processTool("call-1")}
        workers={{ ...NO_WORKERS, background: () => view }}
      />,
      { wrapper: withApp(fixtureApp([view])) },
    );
    await waitFor(() => expect(screen.getAllByText(/running · 42s/).length).toBeGreaterThan(0));
  });

  it("renders stable terminal elapsed anchored to completed_at", async () => {
    const view = bgView({
      toolCallId: "call-2",
      lifecycleState: "completed",
      waitStatus: {
        lifecycle: "completed",
        lifecycleStateRaw: "completed",
        startedAt: "2026-05-20T12:00:00Z",
        completedAt: "2026-05-20T12:01:30Z",
        runningElapsedSeconds: null,
        terminalElapsedSeconds: 90,
      },
    });
    render(
      <WorkerStep
        tool={processTool("call-2", "success")}
        workers={{ ...NO_WORKERS, background: () => view }}
      />,
      { wrapper: withApp(fixtureApp([view])) },
    );
    await waitFor(() => expect(screen.getAllByText(/90s/).length).toBeGreaterThan(0));
  });

  it("renders unknown lifecycle without relabeling it running", async () => {
    const view = bgView({
      toolCallId: "call-3",
      lifecycleState: "garbage",
      waitStatus: {
        lifecycle: "unknown",
        lifecycleStateRaw: "garbage",
        startedAt: null,
        completedAt: null,
        runningElapsedSeconds: null,
        terminalElapsedSeconds: null,
      },
    });
    render(
      <WorkerStep
        tool={processTool("call-3")}
        workers={{ ...NO_WORKERS, background: () => view }}
      />,
      { wrapper: withApp(fixtureApp([view])) },
    );
    await waitFor(() => expect(screen.getAllByText(/state unknown/).length).toBeGreaterThan(0));
  });
});
