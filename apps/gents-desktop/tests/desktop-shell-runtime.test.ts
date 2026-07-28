import { describe, expect, it, vi } from "vitest";

import { createTrailingRefreshQueue } from "../src/hooks/desktopShellRuntime";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("createTrailingRefreshQueue", () => {
  it("collapses an update burst into one active and one trailing refresh", async () => {
    const passes = [deferred(), deferred()];
    let active = 0;
    let maxActive = 0;
    const refresh = vi.fn(async () => {
      const pass = passes[refresh.mock.calls.length - 1];
      active += 1;
      maxActive = Math.max(maxActive, active);
      await pass.promise;
      active -= 1;
    });
    const queue = createTrailingRefreshQueue(refresh);

    const first = queue.request();
    const second = queue.request();
    const third = queue.request();
    expect(refresh).toHaveBeenCalledTimes(1);

    passes[0].resolve();
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(2));
    expect(maxActive).toBe(1);

    passes[1].resolve();
    await Promise.all([first, second, third]);
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(maxActive).toBe(1);
  });

  it("drops a queued trailing refresh when disposed", async () => {
    const pass = deferred();
    const refresh = vi.fn(() => pass.promise);
    const queue = createTrailingRefreshQueue(refresh);

    const active = queue.request();
    void queue.request();
    queue.dispose();
    pass.resolve();
    await active;

    expect(refresh).toHaveBeenCalledTimes(1);
    await queue.request();
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
