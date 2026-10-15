import { testApp } from "./app-fixture";
import { describe, expect, it, vi } from "vitest";

import { createTaskActions } from "../src/hooks/taskActions";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createSelectionStore, selection } from "../src/hooks/selectionStore";

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
  const store = createSelectionStore();
  const effects = {
    refreshSnapshot: vi.fn(async () => undefined),
    reportFailure: vi.fn(),
  };
  const actions = createTaskActions({
    ...effects,
    api: { runTask, runSchedule } as unknown as DesktopApiAdapter,
    store,
  });
  return {
    actions,
    store,
    advanceIntent: () => selection.advanceIntent(store),
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
    const running = f.actions.runTask({ taskId: "task-a", args: {} });
    f.advanceIntent();
    pending.resolve({ requestId: "request-a", sessionId: "session-a" });

    await expect(running).resolves.toEqual({
      requestId: "request-a",
      sessionId: "session-a",
    });
    expect(f.refreshSnapshot).toHaveBeenCalledOnce();
    expect(f.store.getState().sessionId).toBeNull();
  });

  it("does not publish a stale schedule failure into the current intent", async () => {
    const pending = deferred<never>();
    const f = fixture(
      async () => null,
      () => pending.promise,
    );
    const running = f.actions.runSchedule({ scheduleId: "schedule-a" });
    f.advanceIntent();
    pending.reject(new Error("old schedule failed"));

    await expect(running).rejects.toThrow("old schedule failed");
    expect(f.reportFailure).not.toHaveBeenCalled();
    expect(f.store.getState().sessionId).toBeNull();
  });

  it("does not navigate to the result session while the run intent is current", async () => {
    const f = fixture(async () => ({
      requestId: "request-current",
      sessionId: "session-current",
    }));

    await f.actions.runTask({ taskId: "task-a", args: {} });

    expect(f.store.getState().sessionId).toBeNull();
  });

  it("preserves an accepted run when the read after it fails", async () => {
    const reportFailure = vi.fn();
    const app = testApp({
      api: {
        runTask: vi.fn().mockResolvedValue({
          requestId: "accepted-request",
          sessionId: "accepted-session",
        }),
        fetchDesktopSnapshot: vi
          .fn()
          .mockRejectedValue(new Error("observation unavailable")),
      },
      reportFailure,
    });

    await expect(app.actions.runTask({ taskId: "task-a", args: {} })).resolves.toEqual({
      requestId: "accepted-request",
      sessionId: "accepted-session",
    });
    expect(reportFailure).not.toHaveBeenCalled();
    /* the failed read is the client's own state, in the banner */
    expect(app.stores.client.getState().error).toContain("observation unavailable");
  });
});
