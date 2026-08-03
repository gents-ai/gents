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
});
