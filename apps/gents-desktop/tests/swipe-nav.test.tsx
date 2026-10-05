import { render, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SwipeHandles } from "../src/ui/app/SwipeHandles";
import { COMMIT, scrollRoom, useSwipeNav } from "../src/ui/lib/swipe-nav";

type Progress = { amount: number; phase: "moving" | "ended" | "cancelled" };

const native = vi.hoisted(() => ({
  handler: null as null | ((event: { payload: Progress }) => void),
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    listen: (_event: string, handler: (event: { payload: Progress }) => void) => {
      native.handler = handler;
      return Promise.resolve(() => {
        native.handler = null;
      });
    },
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));

const swipe = (amount: number, phase: Progress["phase"]) =>
  native.handler?.({ payload: { amount, phase } });

const history = (canBack = true, canForward = true) => ({
  canBack,
  canForward,
  back: vi.fn(),
  forward: vi.fn(),
});

function scroller(width: number, room: number, left: number, overflowX = "auto") {
  const el = document.createElement("div");
  el.style.overflowX = overflowX;
  Object.defineProperty(el, "clientWidth", { value: width });
  Object.defineProperty(el, "scrollWidth", { value: width + room });
  el.scrollLeft = left;
  document.body.append(el);
  return el;
}

beforeEach(() => {
  Object.defineProperty(navigator, "platform", {
    value: "MacIntel",
    configurable: true,
  });
  Object.defineProperty(navigator, "maxTouchPoints", { value: 0, configurable: true });
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  native.invoke.mockClear();
});

afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  document.body.innerHTML = "";
});

describe("scrollRoom", () => {
  it("reports the sides a sideways scroller can still move toward", () => {
    const el = scroller(200, 300, 0);
    expect(scrollRoom(el)).toEqual({ left: false, right: true });
    el.scrollLeft = 300;
    expect(scrollRoom(el)).toEqual({ left: true, right: false });
    el.scrollLeft = 120;
    expect(scrollRoom(el)).toEqual({ left: true, right: true });
  });

  it("finds the scroller from content inside it", () => {
    const el = scroller(200, 300, 0);
    const line = document.createElement("span");
    el.append(line);
    expect(scrollRoom(line)).toEqual({ left: false, right: true });
  });

  it("ignores overflow that does not scroll", () => {
    expect(scrollRoom(scroller(200, 300, 0, "hidden"))).toEqual({
      left: false,
      right: false,
    });
  });
});

describe("useSwipeNav", () => {
  it("goes back once the swipe passes the commit distance", async () => {
    const h = history();
    renderHook(() => useSwipeNav(h));
    await vi.waitFor(() => expect(native.handler).not.toBeNull());
    swipe(COMMIT / 2, "moving");
    swipe(COMMIT, "moving");
    swipe(COMMIT, "ended");
    expect(h.back).toHaveBeenCalledTimes(1);
    expect(h.forward).not.toHaveBeenCalled();
  });

  it("goes forward on a swipe the other way", async () => {
    const h = history();
    renderHook(() => useSwipeNav(h));
    await vi.waitFor(() => expect(native.handler).not.toBeNull());
    swipe(-COMMIT, "moving");
    swipe(-COMMIT, "ended");
    expect(h.forward).toHaveBeenCalledTimes(1);
  });

  it("stays put when the fingers lift short of the commit distance", async () => {
    const h = history();
    renderHook(() => useSwipeNav(h));
    await vi.waitFor(() => expect(native.handler).not.toBeNull());
    swipe(COMMIT * 0.9, "moving");
    swipe(COMMIT * 0.9, "ended");
    swipe(COMMIT, "cancelled");
    expect(h.back).not.toHaveBeenCalled();
  });

  it("does nothing toward a side with no entry", async () => {
    const h = history(false, false);
    renderHook(() => useSwipeNav(h));
    await vi.waitFor(() => expect(native.handler).not.toBeNull());
    swipe(1, "ended");
    swipe(-1, "ended");
    expect(h.back).not.toHaveBeenCalled();
    expect(h.forward).not.toHaveBeenCalled();
  });

  it("fills the handle solid at the commit distance and clears it on lift", async () => {
    render(<SwipeHandles />);
    renderHook(() => useSwipeNav(history()));
    await vi.waitFor(() => expect(native.handler).not.toBeNull());
    const chip = document.querySelector<HTMLElement>('[data-swipe-handle="back"]')!;
    swipe(COMMIT / 2, "moving");
    expect(chip.dataset.armed).toBeUndefined();
    swipe(COMMIT, "moving");
    expect(chip.dataset.armed).toBe("");
    swipe(COMMIT, "ended");
    expect(chip.dataset.armed).toBeUndefined();
    expect(chip.style.transform).toBe("");
  });

  it("tells native the scroll room under the pointer, only when it changes", async () => {
    const el = scroller(200, 300, 0);
    document.elementFromPoint = () => el;
    renderHook(() => useSwipeNav(history()));
    window.dispatchEvent(new PointerEvent("pointermove", { clientX: 10, clientY: 10 }));
    await vi.waitFor(() =>
      expect(native.invoke).toHaveBeenCalledWith("swipe_scroll_edges", {
        left: false,
        right: true,
      }),
    );
    window.dispatchEvent(new PointerEvent("pointermove", { clientX: 12, clientY: 10 }));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    expect(native.invoke).toHaveBeenCalledTimes(1);
    el.scrollLeft = 300;
    el.dispatchEvent(new Event("scroll"));
    await vi.waitFor(() =>
      expect(native.invoke).toHaveBeenLastCalledWith("swipe_scroll_edges", {
        left: true,
        right: false,
      }),
    );
  });

  it("stays off on an iPad, which reports a Mac platform", async () => {
    Object.defineProperty(navigator, "maxTouchPoints", {
      value: 5,
      configurable: true,
    });
    renderHook(() => useSwipeNav(history()));
    await Promise.resolve();
    expect(native.handler).toBeNull();
  });

  it("stays off outside macOS", async () => {
    Object.defineProperty(navigator, "platform", {
      value: "Win32",
      configurable: true,
    });
    renderHook(() => useSwipeNav(history()));
    await Promise.resolve();
    expect(native.handler).toBeNull();
  });
});
