import { readFileSync } from "node:fs";
import { join } from "node:path";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  CausedRequestView,
  RenderedToolCallView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { SubagentList, WorkerStep, workerNow } from "../src/ui/screens/WorkerStep";
import { NO_WORKERS, type Subagent, type Workers } from "../src/ui/screens/workers";
import { WorkerActionsContext } from "../src/ui/screens/WorkerActions";

const start = (statusKind = "success"): RenderedToolCallView =>
  ({
    itemKey: "tool-1",
    toolName: "create_session",
    toolCallId: "call-1",
    requestId: "parent-req",
    statusKind,
    awaitMode: "background",
    presentation: {
      kind: "subagent",
      action: "start",
      name: "reviewer",
      sessionId: "child-session",
      description: "Review the diff",
      output: null,
    },
  }) as unknown as RenderedToolCallView;

/* what the bridge sends in production: message_count is always None */
const summary = (turnState: string | null): SessionSummary =>
  ({
    sessionId: "child-session",
    title: "Reviewer",
    behaviorId: null,
    turnState,
    latestRequestId: "child-req-2",
    messageCount: null,
    updatedAt: null,
  }) as unknown as SessionSummary;

const caused = (requestId: string, lifecycleState: string): CausedRequestView => ({
  requestId,
  requestDocId: `doc-${requestId}`,
  sessionId: "child-session",
  agentDid: "did:key:reviewer",
  behaviorId: null,
  lifecycleState,
  interruptRequestedAt: null,
  createdAt: null,
  hop: 1,
  causedByRequestId: "parent-req",
  causedByRequestDocId: "doc-parent-req",
  causedByToolCallId: "call-1",
  causedBySessionId: "parent-session",
});

const subagent = (
  turnState: string | null,
  live: CausedRequestView | null = null,
): Subagent => ({
  sessionId: "child-session",
  agentDid: "did:key:reviewer",
  summary: summary(turnState),
  requests: [caused("child-req", "completed"), ...(live ? [live] : [])],
  live,
});

const reached = (s: Subagent) => ({ subagent: s, request: s.requests[0]! });

function workersWith(s: Subagent | null): Workers {
  return {
    ...NO_WORKERS,
    all: s ? [s] : [],
    byToolCall: () => (s ? reached(s) : null),
    loaded: true,
  };
}

function renderStep(s: Subagent | null, stop = vi.fn()) {
  render(
    <WorkerActionsContext.Provider value={{ stop }}>
      <WorkerStep tool={start()} workers={workersWith(s)} />
    </WorkerActionsContext.Provider>,
  );
  return stop;
}

describe("subagent state", () => {
  it("never infers replication from a missing message count", () => {
    for (const turn of ["running", "completed", "waitingForClaim"]) {
      expect(workerNow(start(), reached(subagent(turn))).text).not.toMatch(/sync/i);
    }
  });

  it("reads every live turn state the bridge emits as running", () => {
    expect(workerNow(start(), reached(subagent("running"))).tone).toBe("running");
    expect(workerNow(start(), reached(subagent("waitingForClaim")))).toEqual({
      tone: "running",
      text: "waiting for the agent to pick it up",
    });
  });

  it("uses the caused request's lifecycle when there is no summary", () => {
    const lifecycle = (lifecycleState: string) => {
      const request = caused("child-req", lifecycleState);
      return workerNow(start(), {
        subagent: {
          sessionId: "child-session",
          agentDid: null,
          summary: null,
          requests: [request],
          live: null,
        },
        request,
      });
    };
    expect(lifecycle("processing").tone).toBe("running");
    expect(lifecycle("pending").text).toBe("waiting for the agent to pick it up");
    expect(lifecycle("completed").tone).toBe("done");
    expect(lifecycle("dead").tone).toBe("failed");
  });

  it("settles on the terminal turn states", () => {
    expect(workerNow(start(), reached(subagent("completed"))).tone).toBe("done");
    expect(workerNow(start(), reached(subagent("failed"))).tone).toBe("failed");
    expect(workerNow(start(), reached(subagent("interrupted"))).tone).toBe("stopped");
    expect(workerNow(start(), reached(subagent("superseded"))).tone).toBe("stopped");
  });

  it("offers Stop on a running subagent and targets only its live request", () => {
    const live = caused("child-req-2", "processing");
    const stop = renderStep(subagent("running", live));
    expect(screen.queryByText(/not synced/)).toBeNull();
    screen.getByRole("button", { name: "Stop Reviewer" }).click();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(stop).toHaveBeenCalledWith(live);
  });

  it("offers Stop while a subagent waits for the agent", () => {
    renderStep(subagent("waitingForClaim", caused("child-req-2", "pending")));
    expect(
      screen.getAllByText("waiting for the agent to pick it up").length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Stop Reviewer" })).toBeInTheDocument();
  });

  it("offers no Stop once the subagent has settled", () => {
    renderStep(subagent("completed"));
    expect(screen.queryByRole("button", { name: "Stop Reviewer" })).toBeNull();
  });

  it("keeps replication claims out of the transcript, which has no owner for them", () => {
    for (const file of ["SessionScreen.tsx", "WorkerStep.tsx", "workers.ts"]) {
      const source = readFileSync(join(__dirname, "../src/ui/screens", file), "utf8");
      expect(source, file).not.toMatch(/has not replicated to this desktop/);
      expect(source, file).not.toMatch(/not synced to this desktop/);
      expect(source, file).not.toMatch(/messageCount == null/);
    }
  });

  it("never reads the tool call's own status as a live subagent", () => {
    expect(workerNow(start("success"), null)).toEqual({
      tone: "done",
      text: "started",
    });
    expect(workerNow(start("error"), null).tone).toBe("failed");
    expect(workerNow(start("unknown"), null)).toEqual({
      tone: "unknown",
      text: "state unknown",
    });
    expect(workerNow(start("running"), null)).toEqual({
      tone: "running",
      text: "starting",
    });
  });

  it("offers no Stop without a live request to stop", () => {
    for (const status of ["success", "unknown", "error", "running"]) {
      const view = render(
        <WorkerActionsContext.Provider value={{ stop: vi.fn() }}>
          <WorkerStep tool={start(status)} workers={NO_WORKERS} />
        </WorkerActionsContext.Provider>,
      );
      expect(screen.queryByRole("button", { name: /^Stop / }), status).toBeNull();
      view.unmount();
    }
  });

  it("links a subagent row to the session it reached, as an ordinary session", () => {
    renderStep(subagent("completed"));
    expect(screen.getByRole("link", { name: "Open Reviewer" })).toBeInTheDocument();
  });

  it("labels a message to an existing session as one", () => {
    const message = {
      ...start(),
      toolName: "send_message",
      presentation: { ...start().presentation, action: "message" },
    } as RenderedToolCallView;
    render(<WorkerStep tool={message} workers={workersWith(subagent("running"))} />);
    expect(screen.getByText("Messaged")).toBeInTheDocument();
  });
});

describe("subagent list", () => {
  it("lists each subagent session with its state, a way in and Stop while it works", () => {
    const stop = vi.fn();
    const live = caused("child-req-2", "processing");
    render(
      <WorkerActionsContext.Provider value={{ stop }}>
        <SubagentList workers={workersWith(subagent("running", live))} />
      </WorkerActionsContext.Provider>,
    );
    expect(screen.getByText("Subagents")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Reviewer/ })).toBeInTheDocument();
    screen.getByRole("button", { name: "Stop Reviewer" }).click();
    expect(stop).toHaveBeenCalledWith(live);
  });

  it("renders nothing for a session that reached no other session", () => {
    const view = render(<SubagentList workers={NO_WORKERS} />);
    expect(view.container).toBeEmptyDOMElement();
  });
});
