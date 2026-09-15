import { describe, expect, it, vi } from "vitest";

import { createDesktopShellTaskActions } from "../src/hooks/desktopShellTaskActions";
import { acceptsAsyncResult } from "../src/hooks/desktopShellRuntime";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function fixture(runTask: () => Promise<unknown>, runSchedule = runTask) {
  let generation = 0;
  const effects = {
    refreshSession: vi.fn(async () => null),
    refreshSnapshot: vi.fn(async () => undefined),
    setError: vi.fn(),
    setRunningTask: vi.fn(),
    setSavingConfig: vi.fn(),
    setSelectedSessionId: vi.fn(),
  };
  const actions = createDesktopShellTaskActions({
    ...effects,
    acceptsComposeIntent: (captured: number) =>
      acceptsAsyncResult(generation, captured),
    advanceComposeIntent: () => {
      generation += 1;
    },
    api: { runTask, runSchedule },
    beginSnapshotPublication: vi.fn(),
    captureComposeIntent: () => generation,
    runningTaskCountRef: { current: 0 },
  } as unknown as Parameters<typeof createDesktopShellTaskActions>[0]);
  return {
    actions,
    advanceIntent: () => {
      generation += 1;
    },
    ...effects,
  };
}

describe("task and schedule async intent ordering", () => {
  it("keeps a stale task acceptance durable without selecting its session", async () => {
    const pending = deferred<{
      requestId: string;
      sessionId: string;
    }>();
    const f = fixture(() => pending.promise);
    const running = f.actions.onRunTask({ taskId: "task-a", args: {} });
    f.advanceIntent();
    pending.resolve({ requestId: "request-a", sessionId: "session-a" });

    await expect(running).resolves.toEqual({
      requestId: "request-a",
      sessionId: "session-a",
    });
    expect(f.refreshSnapshot).toHaveBeenCalledOnce();
    expect(f.setSelectedSessionId).not.toHaveBeenCalled();
    expect(f.refreshSession).not.toHaveBeenCalled();
  });

  it("does not publish a stale schedule failure into the current intent", async () => {
    const pending = deferred<never>();
    const f = fixture(
      async () => null,
      () => pending.promise,
    );
    const running = f.actions.onRunSchedule({ scheduleId: "schedule-a" });
    f.advanceIntent();
    pending.reject(new Error("old schedule failed"));

    await expect(running).rejects.toThrow("old schedule failed");
    expect(f.setError).toHaveBeenCalledTimes(1);
    expect(f.setError).toHaveBeenCalledWith(null);
    expect(f.setSelectedSessionId).not.toHaveBeenCalled();
  });

  it("selects and hydrates the session while the run intent is current", async () => {
    const f = fixture(async () => ({
      requestId: "request-current",
      sessionId: "session-current",
    }));

    await f.actions.onRunTask({ taskId: "task-a", args: {} });

    expect(f.setSelectedSessionId).toHaveBeenCalledWith("session-current");
    expect(f.refreshSession).toHaveBeenCalledWith("session-current");
  });

  it("keeps running state active until every overlapping run completes", async () => {
    const task = deferred<{ requestId: string }>();
    const schedule = deferred<{ requestId: string }>();
    const f = fixture(
      () => task.promise,
      () => schedule.promise,
    );
    const taskRun = f.actions.onRunTask({ taskId: "task-a", args: {} });
    const scheduleRun = f.actions.onRunSchedule({ scheduleId: "schedule-a" });
    expect(f.setRunningTask).toHaveBeenLastCalledWith(true);

    task.resolve({ requestId: "task-request" });
    await taskRun;
    expect(f.setRunningTask).not.toHaveBeenCalledWith(false);

    schedule.resolve({ requestId: "schedule-request" });
    await scheduleRun;
    expect(f.setRunningTask).toHaveBeenLastCalledWith(false);
  });

  it("preserves an accepted mutation when its observation refresh fails", async () => {
    const f = fixture(async () => ({
      requestId: "accepted-request",
      sessionId: "accepted-session",
    }));
    f.refreshSnapshot.mockRejectedValueOnce(new Error("observation unavailable"));

    await expect(f.actions.onRunTask({ taskId: "task-a", args: {} })).resolves.toEqual({
      requestId: "accepted-request",
      sessionId: "accepted-session",
    });
    expect(f.setError).toHaveBeenCalledTimes(1);
    expect(f.setError).toHaveBeenCalledWith(null);
  });
});
