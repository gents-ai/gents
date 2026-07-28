import { afterEach, describe, expect, it, vi } from "vitest";

import {
  listenToDesktopClientUpdates,
  setDesktopClientUpdatedListenerFactoryForTests,
  type DesktopClientUpdatedHandler,
} from "./events.js";

afterEach(() => {
  setDesktopClientUpdatedListenerFactoryForTests(null);
});

describe("listenToDesktopClientUpdates", () => {
  it("prefers an instance-bound listener over the process-global test seam", async () => {
    const globalFactory = vi.fn(async () => vi.fn());
    const instanceCleanup = vi.fn();
    const instanceFactory = vi.fn(async () => instanceCleanup);
    setDesktopClientUpdatedListenerFactoryForTests(globalFactory);

    const unlisten = await listenToDesktopClientUpdates(
      vi.fn(),
      undefined,
      instanceFactory,
    );

    expect(instanceFactory).toHaveBeenCalledOnce();
    expect(globalFactory).not.toHaveBeenCalled();
    unlisten();
    expect(instanceCleanup).toHaveBeenCalledOnce();
  });

  it("routes async handler failures without leaking unhandled rejections", async () => {
    let registered: DesktopClientUpdatedHandler | null = null;
    const cleanup = vi.fn();
    const onError = vi.fn();
    setDesktopClientUpdatedListenerFactoryForTests(async (handler) => {
      registered = handler;
      return cleanup;
    });

    const unlisten = await listenToDesktopClientUpdates(async () => {
      throw new Error("refresh failed");
    }, onError);
    expect(registered).not.toBeNull();

    await registered!({ reason: "store" });
    expect(onError).toHaveBeenCalledWith(
      expect.objectContaining({ message: "refresh failed" }),
    );

    unlisten();
    expect(cleanup).toHaveBeenCalledOnce();
  });
});
