import { describe, expect, it, vi } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import {
  createProjectionController,
  desktopUpdateRefreshScope,
} from "../src/hooks/projectionController";
import { deferred } from "./deferred";

function controller(
  overrides: Partial<Parameters<typeof createProjectionController>[0]> = {},
) {
  return createProjectionController({
    currentSessionId: () => "session-1",
    refreshSnapshot: vi.fn(async () => {}),
    refreshSession: vi.fn(async () => null),
    rereadSession: vi.fn(async () => null),
    readSessionUpdate: vi.fn(async (): Promise<number | null> => null),
    ...overrides,
  });
}

describe("createProjectionController", () => {
  it("settles a coalesced update burst once its shared snapshot refresh has finished", async () => {
    const session = deferred();
    const fleet = deferred();
    const refreshSession = vi.fn(async () => {
      await session.promise;
      return null;
    });
    const refreshSnapshot = vi.fn(async () => fleet.promise);
    const projection = controller({ refreshSession, refreshSnapshot });
    let settled = false;
    const first = projection.request("full").then(() => {
      settled = true;
    });
    await vi.waitFor(() => expect(refreshSession).toHaveBeenCalledTimes(1));
    for (let index = 0; index < 256; index++) {
      void projection.request("full");
    }

    session.resolve();
    await vi.waitFor(() => expect(refreshSession).toHaveBeenCalledTimes(2));
    expect(settled).toBe(false);
    fleet.resolve();
    await first;
    expect(refreshSnapshot).toHaveBeenCalledTimes(2);
  });

  it("coalesces a synchronous delta wave and serializes one trailing wave", async () => {
    const passes = [deferred(), deferred()];
    const readSessionUpdate = vi.fn(async () => {
      const pass = passes[readSessionUpdate.mock.calls.length - 1];
      await pass.promise;
      return null;
    });
    const projection = controller({ readSessionUpdate });

    const first = projection.request("sessionDelta");
    void projection.request("sessionDelta");
    expect(readSessionUpdate).toHaveBeenCalledTimes(0);
    await vi.waitFor(() => expect(readSessionUpdate).toHaveBeenCalledTimes(1));
    void projection.request("sessionDelta");

    passes[0].resolve();
    await vi.waitFor(() => expect(readSessionUpdate).toHaveBeenCalledTimes(2));
    passes[1].resolve();
    await first;
    expect(readSessionUpdate).toHaveBeenCalledTimes(2);
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
      readSessionUpdate: vi.fn(async () => {
        calls.push("delta");
        return null;
      }),
    });

    const delta = projection.request("sessionDelta");
    const full = projection.request("full");
    await Promise.all([delta, full]);

    expect(calls).toEqual(["session", "snapshot"]);
  });

  it("projects the selected chat before a slow fleet refresh on foreground", async () => {
    const fleet = deferred();
    const refreshSession = vi.fn(async () => null);
    const refreshSnapshot = vi.fn(async () => fleet.promise);
    const projection = controller({ refreshSession, refreshSnapshot });

    const refresh = projection.request("full");
    try {
      await vi.waitFor(() => expect(refreshSnapshot).toHaveBeenCalledTimes(1));
      expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
    } finally {
      fleet.resolve();
      await refresh;
    }
  });

  it("keeps session updates moving while a fleet refresh is blocked", async () => {
    const fleet = deferred();
    const refreshSnapshot = vi.fn(async () => fleet.promise);
    const refreshSession = vi.fn(async () => null);
    const projection = controller({ refreshSnapshot, refreshSession });
    const first = projection.request("snapshot");
    await vi.waitFor(() => expect(refreshSnapshot).toHaveBeenCalledTimes(1));
    await projection.request("session");
    expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
    fleet.resolve();
    await first;
  });

  it("coalesces fleet reads without blocking session completion", async () => {
    const fleet = deferred();
    const refreshSnapshot = vi.fn(async () => fleet.promise);
    const refreshSession = vi.fn(
      async () => ({ turnState: "completed" }) as DesktopSessionSnapshot,
    );
    const projection = controller({ refreshSnapshot, refreshSession });
    const first = projection.request("snapshot");
    await vi.waitFor(() => expect(refreshSnapshot).toHaveBeenCalledTimes(1));
    const terminal = projection.request("sessionEvent");
    await vi.waitFor(() => expect(refreshSession).toHaveBeenCalledTimes(1));
    expect(refreshSnapshot).toHaveBeenCalledTimes(1);
    fleet.resolve();
    await Promise.all([first, terminal]);
    expect(refreshSnapshot).toHaveBeenCalledTimes(2);
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
    const readSessionUpdate = vi.fn(async () => 0);
    const refreshSession = vi.fn(async () => null);
    const projection = controller({ readSessionUpdate, refreshSession });

    await projection.request("sessionDelta");

    expect(readSessionUpdate).toHaveBeenCalledTimes(1);
    expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
  });

  it("refreshes the fleet when a delta fallback observes terminal completion", async () => {
    const refreshSnapshot = vi.fn(async () => {});
    const projection = controller({
      refreshSnapshot,
      readSessionUpdate: async () => 0,
      refreshSession: async () =>
        ({ turnState: "completed" }) as DesktopSessionSnapshot,
    });
    await projection.request("sessionDelta");
    expect(refreshSnapshot).toHaveBeenCalledTimes(1);
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

  it("continues a waiting streak only for a re-read no other request joins", async () => {
    const pass = deferred();
    const refreshSession = vi.fn(async () => null);
    const rereadSession = vi.fn(async () => null);
    const readSessionUpdate = vi.fn(async () => {
      await pass.promise;
      return null;
    });
    const projection = controller({
      refreshSession,
      rereadSession,
      readSessionUpdate,
    });
    void projection.request("sessionDelta");
    await vi.waitFor(() => expect(readSessionUpdate).toHaveBeenCalledTimes(1));
    void projection.request("reread");
    const done = projection.request("session");
    pass.resolve();
    await done;
    expect(refreshSession).toHaveBeenCalledTimes(1);
    expect(rereadSession).not.toHaveBeenCalled();
    await projection.request("reread");
    expect(rereadSession).toHaveBeenCalledTimes(1);
    expect(refreshSession).toHaveBeenCalledTimes(1);
  });

  it("drops queued trailing work when disposed", async () => {
    const pass = deferred();
    const readSessionUpdate = vi.fn(async () => {
      await pass.promise;
      return null;
    });
    const refreshSession = vi.fn(async () => null);
    const projection = controller({ readSessionUpdate, refreshSession });

    const active = projection.request("sessionDelta");
    await vi.waitFor(() => expect(readSessionUpdate).toHaveBeenCalledTimes(1));
    void projection.request("session");
    projection.dispose();
    pass.resolve();
    await active;

    expect(refreshSession).not.toHaveBeenCalled();
  });

  it("reads an update it could not read yet in full once the gap ends, never dropping it", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      const readSessionUpdate = vi.fn(async (): Promise<number | null> => 700);
      const refreshSession = vi.fn(async () => null);
      const projection = controller({ readSessionUpdate, refreshSession });

      await projection.request("sessionDelta");
      await projection.request("sessionDelta");
      expect(readSessionUpdate).toHaveBeenCalledTimes(2);
      expect(refreshSession).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(699);
      expect(refreshSession).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(refreshSession).toHaveBeenCalledExactlyOnceWith("session-1");
      await vi.advanceTimersByTimeAsync(5_000);
      expect(refreshSession).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("lets a full read that starts after an update pay the read it was owed", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      const refreshSession = vi.fn(async () => null);
      const projection = controller({
        readSessionUpdate: vi.fn(async (): Promise<number | null> => 700),
        refreshSession,
      });

      await projection.request("sessionDelta");
      await projection.request("session");
      expect(refreshSession).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(5_000);
      expect(refreshSession).toHaveBeenCalledTimes(1);

      await projection.request("sessionDelta");
      projection.dispose();
      await vi.advanceTimersByTimeAsync(5_000);
      expect(refreshSession).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears the bounded React session projection when selection is empty", async () => {
    const refreshSession = vi.fn(async () => null);
    const refreshSnapshot = vi.fn(async () => {});
    const projection = controller({
      currentSessionId: () => null,
      refreshSession,
      refreshSnapshot,
    });

    await projection.request("session");

    expect(refreshSession).toHaveBeenCalledExactlyOnceWith(null);
    expect(refreshSnapshot).toHaveBeenCalledTimes(1);
  });
});

describe("desktopUpdateRefreshScope", () => {
  it("uses ordinary store wakes to probe the canonical live cursor", () => {
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1")).toBe(
      "snapshot",
    );
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1", false)).toBe(
      "snapshot",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1")).toBe(
      "sessionDelta",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", null)).toBe("full");
    expect(desktopUpdateRefreshScope("config", null, null)).toBe("full");
  });

  it("re-reads a session waiting on its node when its pairing changes", () => {
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1", true)).toBe(
      "full",
    );
    expect(desktopUpdateRefreshScope("health", "session-1", null, true)).toBe("full");
    expect(desktopUpdateRefreshScope("health", null, null, true)).toBe("snapshot");
  });
});
