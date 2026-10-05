import { describe, expect, it, vi } from "vitest";

import { createDesktopShellTaskActions } from "../src/hooks/desktopShellTaskActions";
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
    setError: vi.fn(),
  };
  const actions = createDesktopShellTaskActions({
    ...effects,
    api: { runTask, runSchedule } as unknown as DesktopApiAdapter,
    store,
    mutateSnapshot: async <T>(operation: () => Promise<T>) => operation(),
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
    expect(f.setError).toHaveBeenCalledTimes(1);
    expect(f.setError).toHaveBeenCalledWith(null);
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

  it("preserves an accepted mutation when its observation refresh fails", async () => {
    const f = fixture(async () => ({
      requestId: "accepted-request",
      sessionId: "accepted-session",
    }));
    f.refreshSnapshot.mockRejectedValueOnce(new Error("observation unavailable"));

    await expect(f.actions.runTask({ taskId: "task-a", args: {} })).resolves.toEqual({
      requestId: "accepted-request",
      sessionId: "accepted-session",
    });
    expect(f.setError).toHaveBeenCalledTimes(1);
    expect(f.setError).toHaveBeenCalledWith(null);
  });
});
