import { readFileSync } from "node:fs";
import { join } from "node:path";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  RenderedToolCallView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { WorkerStep, workerNow } from "../src/ui/screens/WorkerStep";
import type { WorkerState, Workers } from "../src/ui/screens/workers";
import { WorkerActionsContext } from "../src/ui/screens/WorkerActions";

const spawn = (statusKind = "success"): RenderedToolCallView =>
  ({
    itemKey: "tool-1",
    toolName: "spawn_subagent",
    statusKind,
    childRequestId: "child-req",
    presentation: {
      kind: "subagent",
      action: "spawn",
      name: "reviewer",
      childRequestId: "child-req",
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

const worker = (turnState: string | null, extra: Partial<WorkerState> = {}) =>
  ({
    sessionId: "child-session",
    summary: summary(turnState),
    node: null,
    edge: null,
    background: null,
    ...extra,
  }) as WorkerState;

function renderStep(w: WorkerState | null, cancel = vi.fn()) {
  const workers: Workers = {
    byChildRequest: () => w,
    byToolCall: () => null,
    loaded: true,
  };
  render(
    <WorkerActionsContext.Provider value={{ parentRequestId: "parent", cancel }}>
      <WorkerStep tool={spawn()} workers={workers} />
    </WorkerActionsContext.Provider>,
  );
  return cancel;
}

describe("worker state", () => {
  it("never infers replication from a missing message count", () => {
    for (const turn of ["running", "completed", "waitingForClaim"]) {
      expect(workerNow(spawn(), worker(turn)).text).not.toMatch(/sync/i);
    }
  });

  it("reads every live turn state the bridge emits as running", () => {
    expect(workerNow(spawn(), worker("running")).tone).toBe("running");
    expect(workerNow(spawn(), worker("waitingForClaim"))).toEqual({
      tone: "running",
      text: "waiting for the agent to pick it up",
    });
  });

  it("uses the lineage request lifecycle when there is no summary", () => {
    const lifecycle = (lifecycleState: string) =>
      workerNow(spawn(), {
        sessionId: null,
        summary: null,
        node: { lifecycleState } as WorkerState["node"],
        edge: null,
        background: null,
      });
    expect(lifecycle("processing").tone).toBe("running");
    expect(lifecycle("pending").text).toBe("waiting for the agent to pick it up");
    expect(lifecycle("completed").tone).toBe("done");
    expect(lifecycle("dead").tone).toBe("failed");
  });

  it("settles on the terminal turn states", () => {
    expect(workerNow(spawn(), worker("completed")).tone).toBe("done");
    expect(workerNow(spawn(), worker("failed")).tone).toBe("failed");
    expect(workerNow(spawn(), worker("interrupted")).tone).toBe("stopped");
    expect(workerNow(spawn(), worker("superseded")).tone).toBe("stopped");
  });

  it("offers Stop on a running worker and targets its current request", () => {
    const cancel = renderStep(worker("running"));
    expect(screen.queryByText(/not synced/)).toBeNull();
    screen.getByRole("button", { name: "Stop Reviewer" }).click();
    expect(cancel).toHaveBeenCalledWith("child-req-2");
  });

  it("offers Stop while a worker waits for the agent", () => {
    renderStep(worker("waitingForClaim"));
    expect(
      screen.getAllByText("waiting for the agent to pick it up").length,
    ).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Stop Reviewer" })).toBeInTheDocument();
  });

  it("offers no Stop once the worker has settled", () => {
    renderStep(worker("completed"));
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

  it("never reads the tool call's own status as a live worker", () => {
    expect(workerNow(spawn("success"), null)).toEqual({
      tone: "done",
      text: "finished",
    });
    expect(workerNow(spawn("error"), null).tone).toBe("failed");
    expect(workerNow(spawn("unknown"), null)).toEqual({
      tone: "unknown",
      text: "state unknown",
    });
    expect(workerNow(spawn("running"), null)).toEqual({
      tone: "running",
      text: "starting",
    });
  });

  it("offers no Stop without a live fact about the worker", () => {
    for (const status of ["success", "unknown", "error"]) {
      const workers: Workers = {
        byChildRequest: () => null,
        byToolCall: () => null,
        loaded: false,
      };
      const view = render(
        <WorkerActionsContext.Provider
          value={{ parentRequestId: "parent", cancel: vi.fn() }}
        >
          <WorkerStep tool={spawn(status)} workers={workers} />
        </WorkerActionsContext.Provider>,
      );
      expect(screen.queryByRole("button", { name: /^Stop / }), status).toBeNull();
      view.unmount();
    }
  });
});
