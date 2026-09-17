import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useNativeWindowReadiness } from "../src/hooks/useNativeWindowReadiness";

const native = vi.hoisted(() => ({
  mac: true,
  label: "main",
  invoke: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => native }));
vi.mock("../src/lib/shellPlatform", () => ({ isMacTauriShell: () => native.mac }));

describe("native window onboarding gate", () => {
  beforeEach(() => {
    native.mac = true;
    native.label = "main";
    native.invoke.mockClear();
  });

  it("unlocks creation only after the original view finishes setup", () => {
    const { rerender } = renderHook(({ ready }) => useNativeWindowReadiness(ready), {
      initialProps: { ready: false },
    });
    expect(native.invoke).not.toHaveBeenCalled();
    rerender({ ready: true });
    expect(native.invoke).toHaveBeenCalledExactlyOnceWith(
      "desktop_window_setup_complete",
    );
    rerender({ ready: true });
    expect(native.invoke).toHaveBeenCalledTimes(1);
  });

  it("does not let additional views publish onboarding completion", () => {
    native.label = "gents-view-1";
    renderHook(() => useNativeWindowReadiness(true));
    expect(native.invoke).not.toHaveBeenCalled();
  });

  it("does not invoke native window commands in other shells", () => {
    native.mac = false;
    renderHook(() => useNativeWindowReadiness(true));
    expect(native.invoke).not.toHaveBeenCalled();
  });
});
