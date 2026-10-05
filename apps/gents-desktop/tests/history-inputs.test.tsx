import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useHistoryInputs } from "../src/ui/lib/history-inputs";

const history = () => ({
  canBack: true,
  canForward: true,
  back: vi.fn(),
  forward: vi.fn(),
});

const platform = (value: string) =>
  Object.defineProperty(navigator, "platform", { value, configurable: true });

afterEach(() => platform(""));

describe("useHistoryInputs", () => {
  it("binds Cmd+[ and Cmd+] on macOS", () => {
    platform("MacIntel");
    const h = history();
    renderHook(() => useHistoryInputs(h));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "[", metaKey: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "]", metaKey: true }));
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowLeft", altKey: true }),
    );
    expect(h.back).toHaveBeenCalledTimes(1);
    expect(h.forward).toHaveBeenCalledTimes(1);
  });

  it("binds Alt+Left and Alt+Right elsewhere", () => {
    platform("Win32");
    const h = history();
    renderHook(() => useHistoryInputs(h));
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowLeft", altKey: true }),
    );
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowRight", altKey: true }),
    );
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "[", metaKey: true }));
    expect(h.back).toHaveBeenCalledTimes(1);
    expect(h.forward).toHaveBeenCalledTimes(1);
  });

  it("uses the mouse side buttons on every platform", () => {
    const h = history();
    renderHook(() => useHistoryInputs(h));
    window.dispatchEvent(new MouseEvent("mouseup", { button: 3 }));
    window.dispatchEvent(new MouseEvent("mouseup", { button: 4 }));
    expect(h.back).toHaveBeenCalledTimes(1);
    expect(h.forward).toHaveBeenCalledTimes(1);
  });

  it("leaves a field that is being typed in alone", () => {
    platform("MacIntel");
    const h = history();
    renderHook(() => useHistoryInputs(h));
    const input = document.createElement("textarea");
    document.body.append(input);
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "[", metaKey: true, bubbles: true }),
    );
    expect(h.back).not.toHaveBeenCalled();
    input.remove();
  });

  it("does nothing when that direction has no entry", () => {
    platform("MacIntel");
    const h = { ...history(), canBack: false, canForward: false };
    renderHook(() => useHistoryInputs(h));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "[", metaKey: true }));
    window.dispatchEvent(new MouseEvent("mouseup", { button: 4 }));
    expect(h.back).not.toHaveBeenCalled();
    expect(h.forward).not.toHaveBeenCalled();
  });

  it("keeps its listeners across renders and reads the latest history", () => {
    platform("MacIntel");
    const add = vi.spyOn(window, "addEventListener");
    const first = { ...history(), canBack: false };
    const { rerender } = renderHook(({ h }) => useHistoryInputs(h), {
      initialProps: { h: first },
    });
    const bound = add.mock.calls.length;
    const next = history();
    rerender({ h: next });
    expect(add.mock.calls.length).toBe(bound);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "[", metaKey: true }));
    expect(first.back).not.toHaveBeenCalled();
    expect(next.back).toHaveBeenCalledTimes(1);
    add.mockRestore();
  });
});
