import { describe, expect, it, vi } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { createDesktopProjectionController } from "../src/hooks/desktopProjectionController";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function controller(
  overrides: Partial<Parameters<typeof createDesktopProjectionController>[0]> = {},
) {
  return createDesktopProjectionController({
    currentSessionId: () => "session-1",
    refreshSnapshot: vi.fn(async () => {}),
    refreshSession: vi.fn(async () => null),
    refreshSessionLiveDelta: vi.fn(async () => true),
    ...overrides,
  });
}

describe("createDesktopProjectionController", () => {
  it("coalesces a synchronous delta wave and serializes one trailing wave", async () => {
    const passes = [deferred(), deferred()];
    const refreshSessionLiveDelta = vi.fn(async () => {
      const pass = passes[refreshSessionLiveDelta.mock.calls.length - 1];
      await pass.promise;
      return true;
    });
    const projection = controller({ refreshSessionLiveDelta });

    const first = projection.request("sessionDelta");
    void projection.request("sessionDelta");
    expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(0);
    await vi.waitFor(() => expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(1));
    void projection.request("sessionDelta");

    passes[0].resolve();
    await vi.waitFor(() => expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(2));
    passes[1].resolve();
    await first;
    expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(2);
  });

  it("lets a full session projection supersede a queued live delta", async () => {
    const calls: string[] = [];
    const projection = controller({
      refreshSnapshot: vi.fn(async () => {
        calls.push("snapshot");
      }),
      refreshSession: vi.fn(async () => {
        calls.push("session");
        return null;
      }),
      refreshSessionLiveDelta: vi.fn(async () => {
        calls.push("delta");
        return true;
      }),
    });

    const delta = projection.request("sessionDelta");
    const full = projection.request("full");
    await Promise.all([delta, full]);

    expect(calls).toEqual(["snapshot", "session"]);
  });

  it("still refreshes the bounded session when the fleet snapshot fails", async () => {
    const error = new Error("snapshot unavailable");
    const refreshSession = vi.fn(async () => null);
    const onError = vi.fn();
    const projection = controller({
      refreshSnapshot: vi.fn(async () => {
        throw error;
      }),
      refreshSession,
      onError,
    });

    await projection.request("full");

    expect(onError).toHaveBeenCalledExactlyOnceWith(error);
    expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
  });

  it("promotes a rejected delta to one bounded session projection", async () => {
    const refreshSessionLiveDelta = vi.fn(async () => false);
    const refreshSession = vi.fn(async () => null);
    const projection = controller({ refreshSessionLiveDelta, refreshSession });

    await projection.request("sessionDelta");

    expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(1);
    expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
  });

  it("refreshes the index once after a terminal session event projection", async () => {
    const refreshSnapshot = vi.fn(async () => {});
    const refreshSession = vi.fn(async () => {
      return { turnState: "completed" } as DesktopSessionSnapshot;
    });
    const projection = controller({ refreshSnapshot, refreshSession });

    await projection.request("sessionEvent");

    expect(refreshSession).toHaveBeenCalledTimes(1);
    expect(refreshSnapshot).toHaveBeenCalledTimes(1);
  });

  it("finishes the terminal index refresh even when request tracking disposes", async () => {
    const terminal = deferred();
    const refreshSnapshot = vi.fn(async () => {});
    const refreshSession = vi.fn(async () => {
      await terminal.promise;
      return { turnState: "completed" } as DesktopSessionSnapshot;
    });
    const projection = controller({ refreshSnapshot, refreshSession });

    const request = projection.request("sessionEvent");
    await vi.waitFor(() => expect(refreshSession).toHaveBeenCalledTimes(1));
    projection.dispose();
    terminal.resolve();
    await request;

    expect(refreshSnapshot).toHaveBeenCalledTimes(1);
  });

  it("does not refresh the fleet index when selecting an already terminal session", async () => {
    const refreshSnapshot = vi.fn(async () => {});
    const refreshSession = vi.fn(async () => {
      return { turnState: "completed" } as DesktopSessionSnapshot;
    });
    const projection = controller({ refreshSnapshot, refreshSession });

    await projection.request("session");

    expect(refreshSession).toHaveBeenCalledTimes(1);
    expect(refreshSnapshot).not.toHaveBeenCalled();
  });

  it("drops queued trailing work when disposed", async () => {
    const pass = deferred();
    const refreshSessionLiveDelta = vi.fn(async () => {
      await pass.promise;
      return true;
    });
    const refreshSession = vi.fn(async () => null);
    const projection = controller({ refreshSessionLiveDelta, refreshSession });

    const active = projection.request("sessionDelta");
    await vi.waitFor(() => expect(refreshSessionLiveDelta).toHaveBeenCalledTimes(1));
    void projection.request("session");
    projection.dispose();
    pass.resolve();
    await active;

    expect(refreshSession).not.toHaveBeenCalled();
  });

  it("clears the bounded React session projection when selection is empty", async () => {
    const refreshSession = vi.fn(async () => null);
    const projection = controller({
      currentSessionId: () => null,
      refreshSession,
    });

    await projection.request("session");

    expect(refreshSession).toHaveBeenCalledExactlyOnceWith(null);
  });
});
