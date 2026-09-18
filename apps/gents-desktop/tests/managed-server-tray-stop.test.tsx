import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { useManagedServerTrayStop } from "../src/hooks/useManagedServerTrayStop";

const view = vi.hoisted(() => ({ label: "main", listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => view }));

describe("managed server tray stop ownership", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {},
      configurable: true,
    });
    view.label = "main";
    view.listen.mockReset().mockResolvedValue(vi.fn());
  });
  afterEach(() => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
  });

  it("subscribes only main, through the window-scoped listener", async () => {
    const stopManagedServer = vi.fn().mockResolvedValue({});
    const api = { stopManagedServer } as unknown as DesktopApiAdapter;
    renderHook(() => useManagedServerTrayStop(api));
    view.label = "gents-view-1";
    renderHook(() => useManagedServerTrayStop(api));
    expect(view.listen).toHaveBeenCalledExactlyOnceWith(
      "desktop://managed-server-tray-stop",
      expect.any(Function),
    );
    await act(async () => {
      view.listen.mock.calls[0][1]();
    });
    expect(stopManagedServer).toHaveBeenCalledExactlyOnceWith(true);
  });

  it("cleans up a subscription that resolves after unmount", async () => {
    let resolve!: (cleanup: () => void) => void;
    view.listen.mockReturnValue(
      new Promise<() => void>((done) => {
        resolve = done;
      }),
    );
    const cleanup = vi.fn();
    const { unmount } = renderHook(() =>
      useManagedServerTrayStop({
        stopManagedServer: vi.fn(),
      } as unknown as DesktopApiAdapter),
    );
    unmount();
    await act(async () => {
      resolve(cleanup);
    });
    expect(cleanup).toHaveBeenCalledOnce();
  });

  it("does not subscribe outside Tauri", () => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
    renderHook(() =>
      useManagedServerTrayStop({
        stopManagedServer: vi.fn(),
      } as unknown as DesktopApiAdapter),
    );
    expect(view.listen).not.toHaveBeenCalled();
  });
});
