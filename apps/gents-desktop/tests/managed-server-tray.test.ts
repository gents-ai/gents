import { describe, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import {
  installManagedServerTrayListeners,
  MANAGED_SERVER_TRAY_RESTART_EVENT,
  MANAGED_SERVER_TRAY_START_EVENT,
  MANAGED_SERVER_TRAY_STOP_EVENT,
} from "../src/lib/managedServerTray";

const stopped = {
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
} as const;

describe("managed server tray listeners", () => {
  it("unlistens registrations that resolve after effect teardown", async () => {
    const resolvers: Array<(cleanup: () => void) => void> = [];
    const cleanups = [vi.fn(), vi.fn(), vi.fn()];
    const listen = vi.fn(
      () => new Promise<() => void>((resolve) => resolvers.push(resolve)),
    );
    const api = {
      managedServerStatus: vi.fn(async () => stopped),
    } as unknown as DesktopApiAdapter;

    const teardown = installManagedServerTrayListeners(api, listen, vi.fn());
    teardown();
    resolvers.forEach((resolve, index) => resolve(cleanups[index]));
    await Promise.resolve();

    cleanups.forEach((cleanup) => expect(cleanup).toHaveBeenCalledOnce());
  });

  it("reports missing restart authority and rejected menu actions", async () => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const api = {
      managedServerStatus: vi.fn(async () => stopped),
      restartManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    installManagedServerTrayListeners(
      api,
      async (event, handler) => {
        handlers.set(event, handler);
        return () => {};
      },
      reportError,
    );
    await Promise.resolve();

    handlers.get(MANAGED_SERVER_TRAY_RESTART_EVENT)?.();
    await vi.waitFor(() =>
      expect(reportError).toHaveBeenCalledWith(
        expect.stringContaining("confirmed host access"),
      ),
    );
    expect(api.restartManagedServer).not.toHaveBeenCalled();
  });

  it("opens setup instead of starting without reviewed authority", async () => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const showSetup = vi.fn(async () => {});
    const api = {
      managedServerStatus: vi.fn(async () => stopped),
      startManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    installManagedServerTrayListeners(
      api,
      async (event, handler) => {
        handlers.set(event, handler);
        return () => {};
      },
      reportError,
      showSetup,
    );
    await Promise.resolve();

    handlers.get(MANAGED_SERVER_TRAY_START_EVENT)?.();
    await vi.waitFor(() => expect(showSetup).toHaveBeenCalledOnce());
    expect(api.startManagedServer).not.toHaveBeenCalled();
    expect(reportError).toHaveBeenCalledWith(
      expect.stringContaining("Complete local agent setup"),
    );
  });

  it.each([
    [MANAGED_SERVER_TRAY_STOP_EVENT, "stopManagedServer"],
    [MANAGED_SERVER_TRAY_RESTART_EVENT, "restartManagedServer"],
  ] as const)("rejects %s for a manually started runtime", async (event, command) => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const api = {
      managedServerStatus: vi.fn(async () => ({
        ...stopped,
        state: "external" as const,
        effectiveToolCeiling: "readwrite" as const,
        effectiveToolRoot: "/Users/test",
      })),
      stopManagedServer: vi.fn(),
      restartManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    installManagedServerTrayListeners(
      api,
      async (name, handler) => {
        handlers.set(name, handler);
        return () => {};
      },
      reportError,
    );
    await Promise.resolve();

    handlers.get(event)?.();
    await vi.waitFor(() =>
      expect(reportError).toHaveBeenCalledWith(
        expect.stringContaining("started outside the managed service"),
      ),
    );
    expect(api[command]).not.toHaveBeenCalled();
  });
});
