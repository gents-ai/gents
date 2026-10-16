import { act, fireEvent, renderHook, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { holdRow, useFollowTail, useOlderPages } from "../src/ui/lib/scroll";

/* jsdom has no ResizeObserver */
const observers = new Set<() => void>();
class FakeResizeObserver {
  constructor(private readonly callback: () => void) {}
  observe() {
    observers.add(this.callback);
  }
  disconnect() {
    observers.delete(this.callback);
  }
}
vi.stubGlobal("ResizeObserver", FakeResizeObserver);

function transcriptFixture() {
  const viewport = document.createElement("div");
  viewport.dataset.slot = "scroll-area-viewport";
  viewport.append(document.createElement("div"));

  let scrollHeight = 500;
  Object.defineProperties(viewport, {
    clientHeight: { configurable: true, get: () => 200 },
    scrollHeight: { configurable: true, get: () => scrollHeight },
  });

  return {
    viewport,
    content: viewport.firstElementChild as HTMLElement,
    growTo(height: number) {
      scrollHeight = height;
      act(() => observers.forEach((notify) => notify()));
    },
    setHeight(height: number) {
      scrollHeight = height;
    },
  };
}

function readerScrollsTo(viewport: HTMLElement, top: number) {
  act(() => {
    viewport.dispatchEvent(new Event("wheel"));
    viewport.scrollTop = top;
    viewport.dispatchEvent(new Event("scroll"));
  });
}

const foot = (height: number) => height - 200;

describe("transcript streaming follow", () => {
  it("stays pinned across growth, releases on the reader's scroll up, and relocks near the foot", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));

    expect(fixture.viewport.scrollTop).toBe(foot(500));
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(900);
    expect(fixture.viewport.scrollTop).toBe(foot(900));

    readerScrollsTo(fixture.viewport, 100);
    expect(result.current.atBottom).toBe(false);

    fixture.growTo(1_200);
    expect(fixture.viewport.scrollTop).toBe(100);

    readerScrollsTo(fixture.viewport, foot(1_200) - 10);
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(1_500);
    expect(fixture.viewport.scrollTop).toBe(foot(1_500));
  });

  it("leaves a reader moving up near the foot where they are when the content changes", () => {
    const fixture = transcriptFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    fixture.growTo(600);
    expect(fixture.viewport.scrollTop).toBe(foot(600));
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp" }));
      fixture.viewport.scrollTop = foot(600) - 3;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    fixture.growTo(700);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(fixture.viewport.scrollTop).toBe(foot(600) - 3);
  });

  it("is not moved off the foot by a scroll the reader did not make", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));

    act(() => {
      fixture.viewport.scrollTop = 0;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(800);
    expect(fixture.viewport.scrollTop).toBe(foot(800));
  });

  it("stops following at an upward wheel, before the scroll it makes", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -40 }));
    });
    expect(result.current.atBottom).toBe(false);
  });

  it("puts a following view back at the foot after a change that keeps the height", async () => {
    const fixture = transcriptFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    fixture.viewport.scrollTop = foot(500) - 72;
    await act(async () => {
      fixture.viewport.firstElementChild!.append(document.createElement("p"));
    });
    expect(fixture.viewport.scrollTop).toBe(foot(500));
  });

  it("leaves a scroll the reader did not make where it lands", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.scrollTop = 40;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.viewport.scrollTop).toBe(40);
    expect(result.current.atBottom).toBe(true);
  });

  it("returns to the foot and follows again from the way back", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    readerScrollsTo(fixture.viewport, 0);
    expect(result.current.atBottom).toBe(false);

    act(() => result.current.toBottom());
    expect(result.current.atBottom).toBe(true);
    expect(fixture.viewport.scrollTop).toBe(foot(500));

    fixture.growTo(700);
    expect(fixture.viewport.scrollTop).toBe(foot(700));
  });

  it("follows from the way back taken just after scrolling up", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -40 }));
      fixture.viewport.scrollTop = 0;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(false);

    act(() => {
      result.current.toBottom();
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(700);
    expect(fixture.viewport.scrollTop).toBe(foot(700));
  });

  it.each([
    ["button", false],
    ["button", true],
    ["scroll", false],
    ["scroll", true],
  ] as const)(
    "captures a fresh reading position after returning by %s, scroll before mutation: %s",
    async (resume, scrollFirst) => {
      const fixture = transcriptFixture();
      const row = fixture.viewport.firstElementChild as HTMLElement;
      row.dataset.timelineKey = "reply";
      row.getBoundingClientRect = () =>
        ({
          top: 100 - fixture.viewport.scrollTop,
          bottom: 500 - fixture.viewport.scrollTop,
        }) as DOMRect;
      document.body.append(fixture.viewport);
      const { result, unmount } = renderHook(() =>
        useFollowTail(fixture.viewport, "session-1"),
      );
      readerScrollsTo(fixture.viewport, 100);
      if (resume === "button") act(() => result.current.toBottom());
      else readerScrollsTo(fixture.viewport, foot(500));
      act(() => {
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -40 }));
      });
      const expectedTop = foot(500) - (scrollFirst ? 40 : 0);
      if (scrollFirst) fixture.viewport.scrollTop = expectedTop;
      await act(async () => {
        row.append(document.createElement("span"));
      });
      expect(fixture.viewport.scrollTop).toBe(expectedTop);
      readerScrollsTo(fixture.viewport, foot(500) - 40);
      expect(result.current.atBottom).toBe(false);
      unmount();
      fixture.viewport.remove();
    },
  );

  it("lands at the foot of a scroller that mounts after its subject was chosen", () => {
    const fixture = transcriptFixture();
    const { rerender } = renderHook(
      ({ scroller }) => useFollowTail(scroller, "session-1"),
      { initialProps: { scroller: null as HTMLElement | null } },
    );
    fixture.setHeight(800);
    rerender({ scroller: fixture.viewport });
    expect(fixture.viewport.scrollTop).toBe(foot(800));
  });

  it("starts a new subject at its foot even after the reader scrolled up", () => {
    const fixture = transcriptFixture();
    const { rerender } = renderHook(
      ({ subject }) => useFollowTail(fixture.viewport, subject),
      {
        initialProps: { subject: "a" },
      },
    );
    readerScrollsTo(fixture.viewport, 0);
    fixture.setHeight(700);
    rerender({ subject: "b" });
    expect(fixture.viewport.scrollTop).toBe(foot(700));
  });
});

/* jsdom has no layout: each row reports its own place against the 200px view. */
function readingFixture() {
  const fixture = transcriptFixture();
  const area = document.createElement("div");
  const scrollbar = document.createElement("div");
  scrollbar.dataset.slot = "scroll-area-scrollbar";
  area.append(fixture.viewport, scrollbar);
  document.body.append(area);
  fixture.viewport.getBoundingClientRect = () =>
    ({ top: 0, bottom: 200, left: 0, width: 300, height: 200 }) as DOMRect;
  let above = 300;
  const rows = [
    ["above", () => 0, () => above],
    ["middle", () => above, () => above + 520],
    ["last", () => above + 520, () => above + 700],
  ] as const;
  const content = fixture.viewport.firstElementChild as HTMLElement;
  const els = rows.map(([key, top, bottom]) => {
    const row = document.createElement("div");
    row.dataset.timelineKey = key;
    row.getBoundingClientRect = () =>
      ({
        top: top() - fixture.viewport.scrollTop,
        bottom: bottom() - fixture.viewport.scrollTop,
      }) as DOMRect;
    content.append(row);
    return row;
  });
  fixture.setHeight(1_000);
  return {
    ...fixture,
    area,
    scrollbar,
    last: els[2],
    growAbove(by: number) {
      above += by;
      fixture.growTo(1_000 + (above - 300));
    },
    /* an animated scroll: one event per frame of its flight */
    async animate(from: number, to: number, ms: number) {
      for (let t = 0; t <= ms; t += 50) {
        act(() => {
          fixture.viewport.scrollTop = Math.round(from + ((to - from) * t) / ms);
          fixture.viewport.dispatchEvent(new Event("scroll"));
        });
        await act(async () => vi.advanceTimersByTimeAsync(50));
      }
    },
  };
}

describe("a reader's place through their own scroll", () => {
  beforeEach(() =>
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] }),
  );
  afterEach(() => {
    vi.useRealTimers();
    document.body.replaceChildren();
  });

  it.each([
    [
      "a wheel at the foot",
      (el: HTMLElement) => el.dispatchEvent(new WheelEvent("wheel", { deltaY: 40 })),
    ],
    [
      "a key elsewhere",
      () => window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" })),
    ],
    [
      "a wheel in a nested box",
      (el: HTMLElement) => {
        const box = document.createElement("div");
        el.firstElementChild!.append(box);
        box.dispatchEvent(new WheelEvent("wheel", { deltaY: -40, bubbles: true }));
      },
    ],
  ] as const)("still holds a row opened after %s", async (_, input) => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    expect(fixture.viewport.scrollTop).toBe(800);
    act(() => input(fixture.viewport));
    await act(async () => vi.advanceTimersByTimeAsync(200));
    act(() => holdRow(fixture.last));
    fixture.growAbove(100);
    expect(fixture.viewport.scrollTop).toBe(900);
  });

  it("holds where an animated scroll that outlasts its input window rests", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 450, 600);
    await act(async () => vi.advanceTimersByTimeAsync(200));
    expect(fixture.viewport.scrollTop).toBe(450);
    fixture.growAbove(100);
    expect(fixture.viewport.scrollTop).toBe(550);
  });

  it("holds where an animated scroll that stalls past its input window rests", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 600, 200);
    await act(async () => vi.advanceTimersByTimeAsync(500));
    act(() => {
      fixture.viewport.scrollTop = 450;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    fixture.growAbove(100);
    expect(fixture.viewport.scrollTop).toBe(550);
  });

  it("leaves a stalled animation's resumed move to the reader when the content changes first", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 600, 200);
    await act(async () => vi.advanceTimersByTimeAsync(500));
    fixture.viewport.scrollTop = 450;
    await act(async () => {
      fixture.last.append(document.createElement("span"));
    });
    expect(fixture.viewport.scrollTop).toBe(450);
  });

  it("puts back a clamp onto the foot just after the reader's upward scroll", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 500, 200);
    await act(async () => vi.advanceTimersByTimeAsync(400));
    fixture.setHeight(600);
    fixture.viewport.scrollTop = 400;
    await act(async () => {
      fixture.last.append(document.createElement("span"));
    });
    expect(fixture.viewport.scrollTop).toBe(500);
  });

  it("leaves the tail of an animated scroll to the reader when the reply below grows", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 500, 400);
    fixture.viewport.scrollTop = 450;
    await act(async () => {
      fixture.last.append(document.createElement("span"));
    });
    expect(fixture.viewport.scrollTop).toBe(450);
  });

  it("holds where a scrollbar drag let go after a pause in it", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -300 }));
      fixture.viewport.scrollTop = 500;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    await act(async () => vi.advanceTimersByTimeAsync(200));
    fireEvent.pointerDown(fixture.scrollbar);
    act(() => {
      fixture.viewport.scrollTop = 450;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    await act(async () => vi.advanceTimersByTimeAsync(200));
    fireEvent.pointerUp(window);
    fixture.growAbove(100);
    expect(fixture.viewport.scrollTop).toBe(550);
  });
});

describe("older transcript pages", () => {
  it.each(["before", "with"])(
    "loads on upward navigation only, deduplicates requests and keeps the reader's place when rows land %s the load's end",
    async (order) => {
      const fixture = transcriptFixture();
      fixture.viewport.scrollTop = 300;
      let finish!: () => void;
      let landRows!: () => void;
      const load = vi.fn(async () => {
        if (order === "before") act(landRows);
        await new Promise<void>((resolve) => {
          finish = resolve;
        });
        if (order === "with") landRows();
        return true;
      });
      const { result, unmount } = renderHook(() => {
        const [oldest, setOldest] = useState("row-10");
        landRows = () => {
          fixture.setHeight(900);
          setOldest("row-0");
        };
        return useOlderPages(
          fixture.viewport,
          fixture.content,
          "session-1",
          true,
          load,
          oldest,
        );
      });
      expect(load).not.toHaveBeenCalled();
      act(() => {
        fixture.viewport.scrollTop = 100;
        fixture.viewport.dispatchEvent(new Event("scroll"));
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      });
      expect(load).toHaveBeenCalledTimes(1);
      expect(result.current).toBe(true);
      await act(async () => {
        finish();
        await Promise.resolve();
      });
      const content = fixture.viewport.firstElementChild as HTMLElement;
      expect(fixture.viewport.scrollTop).toBe(100);
      expect(content.style.marginTop).toBe("-400px");
      await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500), {
        timeout: 2500,
      });
      expect(content.style.marginTop).toBe("");
      expect(load).toHaveBeenCalledTimes(1);
      act(() => {
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      });
      expect(load).toHaveBeenCalledTimes(2);
      expect(result.current).toBe(true);
      unmount();
    },
  );

  it("asks for no further page once the reader is three views below the top", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { result, unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(1300);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
    });
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(900), {
      timeout: 2500,
    });
    expect(load).toHaveBeenCalledTimes(1);
    expect(result.current).toBe(false);
    unmount();
  });

  it("keeps rows hidden while wheel events still arrive without moving the view", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    for (let i = 0; i < 6; i += 1) {
      await act(async () => {
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -0.4 }));
        await new Promise((resolve) => setTimeout(resolve, 50));
      });
    }
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("keeps rows hidden while a finger rests on the screen", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    const touch = (type: string, touches: { clientY: number }[]) =>
      fixture.viewport.dispatchEvent(Object.assign(new Event(type), { touches }));
    act(() => {
      touch("touchstart", [{ clientY: 100 }]);
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    await act(() => new Promise((resolve) => setTimeout(resolve, 300)));
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    act(() => {
      touch("touchend", []);
    });
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("keeps rows that landed mid-scroll hidden through event silence shorter than the rest bound", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 400));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("keeps rows that landed after a wheel hidden while its scroll has not begun", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 300));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(300);
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(700), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("restores the hidden margin and retries the write when WebKit drops it", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    let dropWrites = 0;
    Object.defineProperty(fixture.viewport, "scrollTop", {
      configurable: true,
      get: () => native.get!.call(fixture.viewport),
      set: (value: number) => {
        if (dropWrites > 0) {
          dropWrites -= 1;
          return;
        }
        native.set!.call(fixture.viewport, value);
      },
    });
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    dropWrites = 1;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1100));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("treats an unhide write clamped to a grown view's foot as taken", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let clientHeight = 200;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    Object.defineProperties(fixture.viewport, {
      clientHeight: { configurable: true, get: () => clientHeight },
      scrollTop: {
        configurable: true,
        get: () => native.get!.call(fixture.viewport),
        set: (value: number) => {
          const foot = fixture.viewport.scrollHeight - clientHeight;
          native.set!.call(fixture.viewport, Math.max(0, Math.min(value, foot)));
        },
      },
    });
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    clientHeight = 600;
    await waitFor(() => expect(fixture.content.style.marginTop).toBe(""), {
      timeout: 2500,
    });
    expect(fixture.viewport.scrollTop).toBe(300);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1200));
    });
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.viewport.scrollTop).toBe(300);
    unmount();
  });

  it("rounds an unhide of a fractional height to the nearest pixel and takes it", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    Object.defineProperty(fixture.viewport, "scrollTop", {
      configurable: true,
      get: () => native.get!.call(fixture.viewport),
      set: (value: number) => {
        native.set!.call(fixture.viewport, Math.floor(value));
      },
    });
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900.875);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.content.style.marginTop).toBe("-400.875px");
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(501), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 1200));
    });
    expect(fixture.viewport.scrollTop).toBe(501);
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("keeps rows that landed after a key press hidden until its scroll has had its chance", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fireEvent.keyDown(fixture.viewport, { key: "ArrowUp" });
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(300);
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(700), {
      timeout: 2500,
    });
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  it("uncovers rows kept out of sight as soon as the reader reaches the top", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState("row-10");
      landRows = () => {
        fixture.setHeight(900);
        setOldest("row-0");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    await act(async () => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    const content = fixture.viewport.firstElementChild as HTMLElement;
    expect(content.style.marginTop).toBe("-400px");
    act(() => {
      fixture.viewport.scrollTop = 0;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.viewport.scrollTop).toBe(400);
    expect(content.style.marginTop).toBe("");
    unmount();
  });

  it("leaves the next session's content alone when one is opened while a page loads", () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 300;
    const load = vi.fn(() => new Promise<boolean>(() => {}));
    let open!: () => void;
    const { unmount } = renderHook(() => {
      const [shown, setShown] = useState({ subject: "a", oldest: "a-10" });
      open = () => setShown({ subject: "b", oldest: "b-0" });
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        shown.subject,
        true,
        load,
        shown.oldest,
      );
    });
    act(() => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(load).toHaveBeenCalledTimes(1);
    fixture.setHeight(2000);
    fixture.viewport.scrollTop = 1800;
    const styles = new MutationObserver(() => {});
    styles.observe(fixture.content, { attributes: true, attributeFilter: ["style"] });
    act(() => open());
    expect(styles.takeRecords()).toEqual([]);
    expect(fixture.viewport.scrollTop).toBe(1800);
    unmount();
  });

  /* a load that resolves at once can have its start and end batched into
     one commit */
  it("asks for the next page after a load that settled at once", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 0;
    let page = 10;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState(`row-${page}`);
      landRows = () => {
        page -= 1;
        setOldest(`row-${page}`);
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        oldest,
      );
    });
    for (const _ of [1, 2]) {
      await act(async () => {
        fixture.viewport.scrollTop = 0;
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      });
    }
    expect(load).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("leaves the reader where they are when no older rows land", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 100;
    const load = vi.fn(async () => false);
    const { result, unmount } = renderHook(() =>
      useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        true,
        load,
        "row-10",
      ),
    );
    await act(async () => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
    });
    expect(load).toHaveBeenCalledTimes(1);
    expect(fixture.viewport.scrollTop).toBe(100);
    expect(result.current).toBe(false);
    unmount();
  });

  it("ignores late scroll correction after switching sessions and stops at history's start", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 0;
    let finish!: (value: boolean) => void;
    const load = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          finish = resolve;
        }),
    );
    const { rerender, unmount } = renderHook(
      ({ subject, older }) =>
        useOlderPages(
          fixture.viewport,
          fixture.content,
          subject,
          older,
          load,
          "row-10",
        ),
      { initialProps: { subject: "a", older: true } },
    );
    act(() => fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 })));
    expect(load).toHaveBeenCalledTimes(1);
    rerender({ subject: "b", older: false });
    await act(async () => {
      fixture.setHeight(900);
      finish(true);
      await Promise.resolve();
    });
    expect(fixture.viewport.scrollTop).toBe(0);
    act(() => fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 })));
    expect(load).toHaveBeenCalledTimes(1);
    unmount();
  });
});

describe("short transcript upward intent", () => {
  it.each(["touch", "keyboard"])(
    "loads older content through %s at the top",
    async (input) => {
      const fixture = transcriptFixture();
      const load = vi.fn(async () => false);
      const { unmount } = renderHook(() =>
        useOlderPages(fixture.viewport, fixture.content, "a", true, load, "row-10"),
      );
      expect(load).not.toHaveBeenCalled();
      await act(async () => {
        if (input === "touch") {
          fireEvent.touchStart(fixture.viewport, { touches: [{ clientY: 50 }] });
          fireEvent.touchMove(fixture.viewport, { touches: [{ clientY: 80 }] });
        } else {
          fireEvent.keyDown(fixture.viewport, { key: "PageUp" });
        }
      });
      expect(load).toHaveBeenCalledTimes(1);
      unmount();
    },
  );
});
