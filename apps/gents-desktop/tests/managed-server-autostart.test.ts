import { describe, expect, it, vi } from "vitest";

import { shouldAutoStartDesktopClient } from "../src/hooks/desktopShellEffects";
import { restoreManagedServer } from "../src/hooks/managedServerLifecycle";
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
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      error: null,
    });

    await restoreManagedServer(api);

    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("leaves an enabled stopped service to the OS manager", async () => {
    const api = apiWithManagedServer({
      state: "stopped",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: null,
      graphql: null,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      error: null,
    });

    await restoreManagedServer(api);

    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("coalesces concurrent service observations", async () => {
    const api = apiWithManagedServer({
      state: "stopped",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: null,
      graphql: null,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      error: null,
    });
    const first = restoreManagedServer(api);
    const second = restoreManagedServer(api);
    await Promise.all([first, second]);

    expect(api.managedServerStatus).toHaveBeenCalledOnce();
    expect(api.startManagedServer).not.toHaveBeenCalled();
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

  it("starts the client on a fresh mobile profile so enrollment can run", () => {
    expect(
      shouldAutoStartDesktopClient(
        {
          bootstrap: {
            clientStateExists: false,
            savedPeers: [],
          },
          client: null,
        } as never,
        false,
        { mobile: true },
      ),
    ).toBe(true);
  });
});
