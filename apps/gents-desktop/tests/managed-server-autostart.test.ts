import { describe, expect, it, vi } from "vitest";

import {
  restoreManagedServer,
  shouldAutoStartDesktopClient,
} from "../src/hooks/desktopShellEffects";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

function apiWithManagedServer(
  status: Awaited<ReturnType<NonNullable<DesktopApiAdapter["managedServerStatus"]>>>,
) {
  return {
    managedServerStatus: vi.fn(async () => status),
    startManagedServer: vi.fn(async () => status),
  } as unknown as DesktopApiAdapter;
}

describe("managed server launch restoration", () => {
  it("does not start a server for a fresh profile", async () => {
    const api = apiWithManagedServer({
      state: "disabled",
      autoStart: false,
      agentName: null,
      agentDid: null,
      graphql: null,
      error: null,
    });

    await restoreManagedServer(api);

    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("restores an opted-in server before client bootstrap", async () => {
    const api = apiWithManagedServer({
      state: "stopped",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: null,
      graphql: null,
      error: null,
    });

    await restoreManagedServer(api);

    expect(api.startManagedServer).toHaveBeenCalledOnce();
    expect(api.startManagedServer).toHaveBeenCalledWith("Workshop Agent");
  });

  it("coalesces concurrent restoration attempts", async () => {
    let releaseStart: (() => void) | undefined;
    const startPending = new Promise<void>((resolve) => {
      releaseStart = resolve;
    });
    const api = apiWithManagedServer({
      state: "stopped",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: null,
      graphql: null,
      error: null,
    });
    vi.mocked(api.startManagedServer).mockImplementation(async () => {
      await startPending;
      return {
        state: "running",
        autoStart: true,
        agentName: "Workshop Agent",
        agentDid: null,
        graphql: null,
        error: null,
      };
    });

    const first = restoreManagedServer(api);
    const second = restoreManagedServer(api);
    await vi.waitFor(() => expect(api.startManagedServer).toHaveBeenCalledOnce());
    releaseStart?.();
    await Promise.all([first, second]);

    expect(api.managedServerStatus).toHaveBeenCalledOnce();
    expect(api.startManagedServer).toHaveBeenCalledOnce();
  });

  it("does not start the client for an uncommitted local peer", () => {
    expect(
      shouldAutoStartDesktopClient(
        {
          bootstrap: {
            savedPeers: [{ source: "local-standard" }],
          },
          client: null,
        } as never,
        false,
      ),
    ).toBe(false);
  });

  it("still reconnects saved remote peers when local hosting was skipped", () => {
    expect(
      shouldAutoStartDesktopClient(
        {
          bootstrap: {
            savedPeers: [{ source: "paired-remote" }],
          },
          client: null,
        } as never,
        false,
      ),
    ).toBe(true);
  });

  it("restarts persisted client state before the first enrollment peer materializes", () => {
    expect(
      shouldAutoStartDesktopClient(
        {
          bootstrap: {
            clientStateExists: true,
            savedPeers: [],
          },
          client: null,
        } as never,
        false,
      ),
    ).toBe(true);
  });
});
