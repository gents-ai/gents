import { readFileSync } from "node:fs";
import { join } from "node:path";
import { screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";
import type {
  CausedRequestView,
  RenderedToolCallView,
  RequestLifecycleState,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import {
  isWorkerStep,
  WorkerStep,
  failureLine,
  workerNow,
} from "../src/ui/screens/WorkerStep";
import {
  NO_WORKERS,
  type Reached,
  type Worker,
  type Workers,
} from "../src/ui/screens/workers";
import { WorkerActionsContext } from "../src/ui/screens/WorkerActions";
import { renderIn, testApp } from "./app-fixture";

const render = (ui: ReactElement) => renderIn(testApp(), ui);

const call = (
  statusKind = "running",
  action: "start" | "message" = "start",
): RenderedToolCallView =>
  ({
    itemKey: "tool-1",
    toolName: action === "start" ? "agent_new" : "agent_message",
    toolCallId: "call-1",
    requestId: "parent-req",
    statusKind,
    awaitMode: "background",
    presentation: {
      kind: "agent",
      action,
      name: "reviewer",
      sessionId: "child-session",
      description: "Review the diff",
      output: null,
    },
  }) as unknown as RenderedToolCallView;

it("keeps session discovery and inspection in ordinary tool steps", () => {
  for (const [toolName, action] of [
    ["agent_list", "list"],
    ["agent_session_list", "sessions"],
    ["agent_session_read", "read"],
  ]) {
    const tool = call("success");
    tool.toolName = toolName;
    tool.awaitMode = "foreground";
    if (tool.presentation.kind !== "agent")
      throw new Error("expected agent presentation");
    tool.presentation.action = action;
    expect(isWorkerStep(tool)).toBe(false);
  }
  expect(isWorkerStep(call("running", "start"))).toBe(true);
  expect(isWorkerStep(call("success", "message"))).toBe(true);
});

const summary = (turnState: string | null): SessionSummary =>
  ({
    sessionId: "child-session",
    title: "Reviewer",
    agentId: null,
    turnState,
    latestRequestId: "child-req-2",
    messageCount: null,
    updatedAt: null,
  }) as unknown as SessionSummary;

const caused = (
  requestId: string,
  lifecycleState: RequestLifecycleState,
): CausedRequestView => ({
  requestId,
  nodeDid: "did:key:reviewer",
  sessionId: "child-session",
  requesterDid: null,
  lifecycleState,
  createdAt: null,
});

const worker = (turnState: string | null): Worker => ({
  sessionId: "child-session",
  nodeDid: "did:key:reviewer",
  summary: summary(turnState),
  link: {
    nodeDid: "did:key:reviewer",
    sessionId: "child-session",
    requesterDid: null,
    causeRequestDocId: "doc-parent-req",
  },
});

const reached = (
  request: CausedRequestView,
  s: Worker | null = worker("running"),
): Reached => ({ request, summary: s?.summary ?? summary("running"), worker: s });

function workersWith(r: Reached | null): Workers {
  return {
    ...NO_WORKERS,
    all: r?.worker ? [r.worker] : [],
    byToolCall: () => r,
    loaded: true,
  };
}

function renderStep(
  tool: RenderedToolCallView,
  r: Reached | null,
  actions = { interrupt: vi.fn() },
) {
  render(
    <WorkerActionsContext.Provider value={actions}>
      <WorkerStep tool={tool} workers={workersWith(r)} />
    </WorkerActionsContext.Provider>,
  );
  return actions;
}

describe("an agent row", () => {
  it("shows the request this row's call caused, not the session's latest", () => {
    expect(
      workerNow(call(), reached(caused("child-req", "completed")), Date.now()),
    ).toEqual({
      tone: "done",
      text: "finished",
    });
    expect(
      workerNow(call(), reached(caused("child-req", "processing")), Date.now()).tone,
    ).toBe("running");
  });

  it("reads the caused request's lifecycle", () => {
    const at = (state: RequestLifecycleState) =>
      workerNow(call(), reached(caused("r", state)), Date.now());
    expect(at("pending").text).toBe("waiting for the agent to pick it up");
    expect(at("processing").tone).toBe("running");
    expect(at("completed").tone).toBe("done");
    expect(at("dead").tone).toBe("failed");
    expect(at("interrupted").tone).toBe("stopped");
    expect(at("superseded").tone).toBe("stopped");
  });

  it("never infers replication from a missing message count", () => {
    for (const state of ["processing", "completed", "pending"] as const) {
      expect(
        workerNow(call(), reached(caused("r", state)), Date.now()).text,
      ).not.toMatch(/sync/i);
    }
  });

  it("never reads the tool call's own status as a live request", () => {
    expect(workerNow(call("success"), null, Date.now())).toEqual({
      tone: "done",
      text: "sent",
    });
    expect(workerNow(call("error"), null, Date.now()).tone).toBe("failed");
    expect(workerNow(call("unknown"), null, Date.now())).toEqual({
      tone: "unknown",
      text: "state unknown",
    });
    expect(workerNow(call("running"), null, Date.now())).toEqual({
      tone: "running",
      text: "starting",
    });
  });

  it("stops by interrupting only the request this row's call caused", () => {
    const request = caused("child-req", "processing");
    const actions = renderStep(call("running"), reached(request));
    screen.getByRole("button", { name: "Stop Reviewer" }).click();
    expect(actions.interrupt).toHaveBeenCalledTimes(1);
    expect(actions.interrupt).toHaveBeenCalledWith(request);
  });

  it("offers no Stop once the caused request is terminal, even while the row runs", () => {
    renderStep(call("running"), reached(caused("child-req", "completed")));
    expect(screen.queryByRole("button", { name: /^Stop / })).toBeNull();
  });

  it("offers no Stop without a caused request to interrupt", () => {
    renderStep(call("running"), null);
    expect(screen.queryByRole("button", { name: /^Stop / })).toBeNull();
  });

  it("labels a start as Started and a message as Messaged", () => {
    renderStep(call("success"), reached(caused("child-req", "completed")));
    expect(screen.getByText("Started")).toBeInTheDocument();
  });

  it("offers no Stop on a session this one did not start, as agent_interrupt refuses", () => {
    const existing = reached(caused("req-existing", "processing"), null);
    const actions = renderStep(call("running", "message"), existing);
    expect(screen.queryByRole("button", { name: /^Stop / })).toBeNull();
    expect(actions.interrupt).not.toBeCalled();
  });

  it("offers Stop on a message to a session this one started", () => {
    const request = caused("child-req-3", "processing");
    const actions = renderStep(call("running", "message"), reached(request));
    screen.getByRole("button", { name: "Stop Reviewer" }).click();
    expect(actions.interrupt).toHaveBeenCalledWith(request);
  });

  it("renders an agent_interrupt call as Interrupted, with no Stop", () => {
    const interrupt = {
      ...call("success"),
      toolName: "agent_interrupt",
      presentation: {
        kind: "agent",
        action: "interrupt",
        name: null,
        sessionId: "child-session",
        description: null,
        output: null,
      },
    } as unknown as RenderedToolCallView;
    renderStep(interrupt, null);
    expect(screen.getByText("Interrupted")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Stop / })).toBeNull();
  });

  it("labels a message to a session this one did not start as Messaged", () => {
    const existing = reached(caused("req-existing", "processing"), null);
    renderStep(call("running", "message"), existing);
    expect(screen.getByText("Messaged")).toBeInTheDocument();
  });

  it("links the row to the session it reached, as an ordinary session", () => {
    renderStep(call("success"), reached(caused("child-req", "completed")));
    expect(screen.getByRole("link", { name: "Open Reviewer" })).toBeInTheDocument();
  });

  it("keeps replication claims out of the transcript, which has no owner for them", () => {
    for (const file of ["SessionScreen.tsx", "WorkerStep.tsx", "workers.ts"]) {
      const source = readFileSync(join(__dirname, "../src/ui/screens", file), "utf8");
      expect(source, file).not.toMatch(/has not replicated to this desktop/);
      expect(source, file).not.toMatch(/not synced to this desktop/);
      expect(source, file).not.toMatch(/messageCount == null/);
    }
  });
});

describe("a background process row", () => {
  it("offers no desktop Stop: killing a native process is not a desktop control", () => {
    const process = {
      itemKey: "proc-1",
      toolName: "spawn_process",
      toolCallId: "call-proc",
      requestId: "parent-req",
      statusKind: "running",
      awaitMode: "background",
      presentation: {
        kind: "process",
        action: "spawn",
        target: "cargo test",
        description: null,
        output: null,
      },
    } as unknown as RenderedToolCallView;
    renderStep(process, null);
    expect(screen.getByText("cargo test")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Stop / })).toBeNull();
  });
});

describe("a failed call's reason", () => {
  it("reads a JSON error's message rather than its opening brace", () => {
    expect(
      failureLine(
        '{\n  "error": "session is neither this agent\'s own nor one it started"\n}',
      ),
    ).toBe("session is neither this agent's own nor one it started");
    expect(failureLine('{"error": {"message": "allowed_agents: [worker]"}}')).toBe(
      "allowed_agents: [worker]",
    );
    expect(failureLine('{\n  "code": 7\n}')).toBeNull();
    expect(failureLine("plain reason\nmore")).toBe("plain reason");
    expect(failureLine("")).toBeNull();
  });
});
