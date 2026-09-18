import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { useManagedServerTrayControls } from "../src/hooks/useManagedServerTrayControls";
import { MANAGED_SERVER_TRAY_STOP_EVENT } from "../src/lib/managedServerTray";

const toastMocks = vi.hoisted(() => ({ error: vi.fn() }));
vi.mock("sonner", () => ({ toast: toastMocks }));

const view = vi.hoisted(() => ({
  label: "main",
  listen: vi.fn(),
  show: vi.fn(),
  setFocus: vi.fn(),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => view }));

const status = {
  state: "running",
  autoStart: true,
  agentName: "Workshop Agent",
  agentDid: null,
  graphql: "http://127.0.0.1:9191/api",
  effectiveToolCeiling: "readwrite",
  effectiveToolRoot: "/Users/test",
  suggestedToolRoot: "/Users/test",
  pairingReady: true,
  error: null,
} as const;

function apiFixture(): DesktopApiAdapter {
  return {
    managedServerStatus: vi.fn(async () => status),
    startManagedServer: vi.fn(),
    stopManagedServer: vi.fn(),
    restartManagedServer: vi.fn(),
  } as unknown as DesktopApiAdapter;
}

describe("managed server tray control ownership", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {},
      configurable: true,
    });
    Object.defineProperty(navigator, "platform", {
      value: "MacIntel",
      configurable: true,
    });
    Object.defineProperty(navigator, "maxTouchPoints", {
      value: 0,
      configurable: true,
    });
    view.label = "main";
    view.listen.mockReset().mockResolvedValue(vi.fn());
    view.show.mockReset().mockResolvedValue(undefined);
    view.setFocus.mockReset().mockResolvedValue(undefined);
    toastMocks.error.mockReset();
  });

  afterEach(() => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
  });

  it("subscribes all controls only in main through window-scoped listeners", async () => {
    const api = apiFixture();
    renderHook(() => useManagedServerTrayControls(api));
    expect(view.listen).toHaveBeenCalledTimes(3);

    view.label = "gents-view-1";
    renderHook(() => useManagedServerTrayControls(api));
    expect(view.listen).toHaveBeenCalledTimes(3);

    const stop = view.listen.mock.calls.find(
      ([event]) => event === MANAGED_SERVER_TRAY_STOP_EVENT,
    );
    await act(async () => {
      stop?.[1]();
    });
    expect(api.managedServerStatus).toHaveBeenCalledOnce();
    expect(api.stopManagedServer).toHaveBeenCalledExactlyOnceWith(false);
    expect(view.show).not.toHaveBeenCalled();
    expect(view.setFocus).not.toHaveBeenCalled();
    expect(toastMocks.error).not.toHaveBeenCalled();
  });

  it("reveals the owner and preserves the command error when focus fails", async () => {
    const api = apiFixture();
    vi.mocked(api.stopManagedServer!).mockRejectedValue(new Error("service busy"));
    view.setFocus.mockRejectedValueOnce(new Error("focus denied"));
    renderHook(() => useManagedServerTrayControls(api));
    const stop = view.listen.mock.calls.find(
      ([event]) => event === MANAGED_SERVER_TRAY_STOP_EVENT,
    );

    await act(async () => {
      stop?.[1]();
    });
    await vi.waitFor(() => expect(view.show).toHaveBeenCalledOnce());
    expect(view.setFocus).toHaveBeenCalledOnce();
    expect(toastMocks.error).toHaveBeenCalledExactlyOnceWith(
      "Agent menu command failed: service busy",
    );
  });

  it("cleans up subscriptions that resolve after unmount", async () => {
    const resolvers: Array<(cleanup: () => void) => void> = [];
    view.listen.mockImplementation(
      () => new Promise<() => void>((resolve) => resolvers.push(resolve)),
    );
    const cleanups = [vi.fn(), vi.fn(), vi.fn()];
    const { unmount } = renderHook(() => useManagedServerTrayControls(apiFixture()));
    unmount();
    await act(async () => {
      resolvers.forEach((resolve, index) => resolve(cleanups[index]));
      await Promise.resolve();
    });
    cleanups.forEach((cleanup) => expect(cleanup).toHaveBeenCalledOnce());
  });

  it("does not subscribe outside Tauri", () => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
    renderHook(() => useManagedServerTrayControls(apiFixture()));
    expect(view.listen).not.toHaveBeenCalled();
  });
});
