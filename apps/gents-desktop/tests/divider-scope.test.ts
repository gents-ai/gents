import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useDivider } from "../src/ui/lib/divider";

const base = {
  key: "divider-scope-test",
  initial: 520,
  min: 320,
  paneMin: 360,
  gap: 8,
  container: 1600,
  rail: 56,
  onClosed: () => {},
};

describe("the dock divider across scopes", () => {
  it("springs open when the dock opens where it is", () => {
    const { result, rerender } = renderHook(
      (props: { open: boolean }) =>
        useDivider({ ...base, scope: "session:a", ...props }),
      {
        initialProps: { open: false },
      },
    );
    rerender({ open: true });
    expect(result.current.settling).toBe(true);
    expect(result.current.pos).toBeLessThan(520);
  });

  it("shows another session's open dock as it stands, without motion", () => {
    const { result, rerender } = renderHook(
      (props: { open: boolean; scope: string }) => useDivider({ ...base, ...props }),
      { initialProps: { open: false, scope: "session:a" } },
    );
    rerender({ open: true, scope: "session:b" });
    expect(result.current.settling).toBe(false);
    expect(result.current.pos).toBe(520);
    rerender({ open: false, scope: "session:a" });
    expect(result.current.settling).toBe(false);
    expect(result.current.pos).toBe(0);
  });
});

describe("the dock divider after unmount", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("stops its spring, so a settle shut does not close the dock", () => {
    const frames = new Map<number, FrameRequestCallback>();
    let next = 0;
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.set(++next, cb);
      return next;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
    const onClosed = vi.fn();
    const { result, rerender, unmount } = renderHook(
      (props: { open: boolean; scope: string }) =>
        useDivider({ ...base, ...props, onClosed }),
      { initialProps: { open: false, scope: "session:a" } },
    );
    rerender({ open: true, scope: "session:b" });
    const pointer = (clientX: number) =>
      ({
        button: 0,
        clientX,
        pointerId: 1,
        preventDefault() {},
        currentTarget: { setPointerCapture() {} },
      }) as unknown as React.PointerEvent<HTMLElement>;
    act(() => result.current.handleProps.onPointerDown(pointer(1000)));
    act(() => result.current.handleProps.onPointerMove(pointer(1510)));
    act(() => result.current.handleProps.onPointerUp());
    expect(result.current.settling).toBe(true);
    expect(frames.size).toBe(1);
    unmount();
    expect(frames.size).toBe(0);
    expect(onClosed).not.toHaveBeenCalled();
  });
});

describe("the dock divider when the window resizes", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("keeps a dock that settled during the resize inside the new range", () => {
    const frames = new Map<number, FrameRequestCallback>();
    let next = 0;
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.set(++next, cb);
      return next;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
    /* runs the spring to rest, a frame at a time */
    const settle = () => {
      for (let t = 16; frames.size > 0 && t < 10_000; t += 16)
        act(() => {
          const due = [...frames];
          frames.clear();
          for (const [, frame] of due) frame(t);
        });
    };
    const { result, rerender } = renderHook(
      (props: { open: boolean; container: number }) =>
        useDivider({ ...base, scope: "session:a", ...props }),
      { initialProps: { open: false, container: 1600 } },
    );
    rerender({ open: true, container: 1600 });
    expect(result.current.settling).toBe(true);
    /* the window narrows while the dock is still opening toward 520 */
    rerender({ open: true, container: 800 });
    settle();

    expect(result.current.settling).toBe(false);
    expect(result.current.pos).toBe(result.current.max);
    expect(result.current.max).toBeLessThan(520);
  });
});
