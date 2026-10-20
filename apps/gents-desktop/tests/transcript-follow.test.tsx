import { act, fireEvent, renderHook } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  holdRow,
  jumpToRow,
  turnFrom,
  useFollowTail,
  useOlderPages,
} from "../src/ui/lib/scroll";

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

  /* the End key is the reader asking for the end: the view follows from
     there even when the browser's own scroll lands short of a foot that
     the reply has moved on in the meantime */
  it("follows from the End key even when the browser's scroll lands short of the foot", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    readerScrollsTo(fixture.viewport, 100);
    expect(result.current.atBottom).toBe(false);
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "End" }));
      /* the browser's scroll, aimed at the foot of a moment ago */
      fixture.viewport.scrollTop = foot(500) - 60;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.viewport.dataset.following).toBe("true");
    fixture.growTo(900);
    expect(fixture.viewport.scrollTop).toBe(foot(900));
  });

  /* a reply growing under a following reader is eased to the foot, a share
     of the way each frame and never less than a few pixels; the content
     moved under the reader without growing is put back at once */
  it("eases growth to the foot, and puts a displaced view back at once", async () => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame"] });
    const fixture = transcriptFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    expect(fixture.viewport.scrollTop).toBe(foot(500));
    fixture.growTo(540);
    expect(fixture.viewport.scrollTop).toBe(foot(500));
    await act(async () => vi.advanceTimersByTimeAsync(16));
    const first = fixture.viewport.scrollTop;
    expect(first).toBeGreaterThan(foot(500));
    expect(first).toBeLessThan(foot(540));
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(foot(540));
    /* the same height, the position moved under the reader: back at once */
    fixture.viewport.scrollTop = foot(540) - 30;
    fixture.growTo(540);
    expect(fixture.viewport.scrollTop).toBe(foot(540));
    vi.useRealTimers();
  });

  /* WebKit eases a held arrow key in a few pixels at a time, so its first
     frames are still within reach of the foot */
  it("leaves a reader moving up near the foot where they are when the content changes", async () => {
    vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame"] });
    const fixture = transcriptFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    /* growth is eased to the foot over a few frames */
    fixture.growTo(600);
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(foot(600));
    vi.useRealTimers();
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
    expect(fixture.viewport.dataset.following).toBe("false");
    /* the way back is offered once there is something below to go back to */
    expect(result.current.atBottom).toBe(true);
    act(() => {
      fixture.viewport.scrollTop = foot(500) - 40;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(false);
  });

  /* a held turn's empty room is not somewhere to go back to */
  it("offers no way back while only a held turn's room lies below", async () => {
    vi.useFakeTimers({
      toFake: [
        "setTimeout",
        "clearTimeout",
        "performance",
        "requestAnimationFrame",
        "cancelAnimationFrame",
      ],
    });
    const fixture = sentTurnFixture();
    const { result, send } = mount(fixture);
    await send();
    await act(async () => vi.advanceTimersByTimeAsync(700));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(result.current.atBottom).toBe(true);
    /* the reply reaches past the foot: now there is */
    fixture.replyTo(520);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(result.current.atBottom).toBe(false);
    vi.useRealTimers();
    document.body.replaceChildren();
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
        top:
          top() -
          fixture.viewport.scrollTop +
          (parseFloat(content.style.marginTop) || 0),
        bottom:
          bottom() -
          fixture.viewport.scrollTop +
          (parseFloat(content.style.marginTop) || 0),
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
    expect(fixture.viewport.scrollTop).toBe(800);
    expect(fixture.last.getBoundingClientRect().top).toBe(20);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(900);
    expect(fixture.last.getBoundingClientRect().top).toBe(20);
  });

  it("shares corrections between a growing held row and an older page", async () => {
    const fixture = readingFixture();
    let finish!: () => void;
    let growPage!: () => void;
    const load = () =>
      new Promise<boolean>((resolve) => {
        finish = () => {
          growPage();
          resolve(true);
        };
      });
    const follow = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    const pages = renderHook(() => {
      const [oldest, setOldest] = useState("first");
      growPage = () => {
        fixture.growAbove(400);
        setOldest("older");
      };
      return useOlderPages(
        fixture.viewport,
        fixture.content,
        "session-1",
        oldest === "first",
        load,
        oldest,
      );
    });
    readerScrollsTo(fixture.viewport, 300);
    const before = fixture.last.getBoundingClientRect().top;
    fixture.growAbove(100);
    await act(async () => finish());
    expect(fixture.viewport.scrollTop).toBe(300);
    expect(fixture.last.getBoundingClientRect().top).toBe(before);
    expect(fixture.content.style.marginTop).toBe("-500px");
    pages.unmount();
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(800);
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.last.getBoundingClientRect().top).toBe(before);
    follow.unmount();
  });

  it("keeps the pending correction deadline while rows continue growing", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    readerScrollsTo(fixture.viewport, 300);
    const before = fixture.last.getBoundingClientRect().top;
    fixture.growAbove(100);
    await act(async () => vi.advanceTimersByTimeAsync(900));
    fixture.growAbove(20);
    expect(fixture.last.getBoundingClientRect().top).toBe(before);
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.viewport.scrollTop).toBe(420);
    expect(fixture.last.getBoundingClientRect().top).toBe(before);
  });

  it("clears a deferred correction when the reader returns to the foot", async () => {
    const fixture = readingFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    readerScrollsTo(fixture.viewport, 300);
    fixture.growAbove(100);
    expect(fixture.content.style.marginTop).toBe("-100px");
    act(() => result.current.toBottom());
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.viewport.scrollTop).toBe(900);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(900);
    expect(result.current.atBottom).toBe(true);
  });

  it("brings a row to the reading inset in one write and holds it there", async () => {
    const fixture = readingFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    expect(result.current.atBottom).toBe(true);
    const middle = fixture.viewport.querySelector('[data-timeline-key="middle"]')!;
    act(() => jumpToRow(middle));

    expect(fixture.viewport.scrollTop).toBe(292);
    expect(middle.getBoundingClientRect().top).toBe(8);
    expect(fixture.viewport.dataset.following).toBe("false");
    fixture.growAbove(100);
    expect(fixture.viewport.scrollTop).toBe(392);
    expect(middle.getBoundingClientRect().top).toBe(8);
  });

  it("makes the jump's write once the reader's scroll has rested", async () => {
    const fixture = readingFixture();
    renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 }));
    });
    await fixture.animate(800, 600, 200);
    const middle = fixture.viewport.querySelector('[data-timeline-key="middle"]')!;
    act(() => jumpToRow(middle));
    expect(fixture.viewport.scrollTop).toBe(600);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(292);
    expect(middle.getBoundingClientRect().top).toBe(8);
  });

  it("moves between the turns the reader sent, from the one on the reading line", () => {
    const fixture = readingFixture();
    for (const row of fixture.viewport.querySelectorAll<HTMLElement>(
      "[data-timeline-key]",
    ))
      row.dataset.turn = "";
    fixture.viewport.scrollTop = 0;
    expect(turnFrom(fixture.viewport, 1)?.getAttribute("data-timeline-key")).toBe(
      "middle",
    );
    expect(turnFrom(fixture.viewport, -1)).toBeNull();
    fixture.viewport.scrollTop = 300;
    expect(turnFrom(fixture.viewport, -1)?.getAttribute("data-timeline-key")).toBe(
      "above",
    );
    expect(turnFrom(fixture.viewport, 1)?.getAttribute("data-timeline-key")).toBe(
      "last",
    );
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
    expect(fixture.viewport.scrollTop).toBe(450);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(550);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
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
    expect(fixture.viewport.scrollTop).toBe(450);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(550);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
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

  /* content shrinking below clamps the view up onto the foot: there is no
     more content to scroll to, so the row moves down by the shrink and
     nothing is hidden above to make up for it, which the browser would
     only clamp away again, more each time */
  it("accepts a clamp onto the foot just after the reader's upward scroll", async () => {
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
    expect(fixture.viewport.scrollTop).toBe(400);
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.last.getBoundingClientRect().top).toBe(420);
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
    expect(fixture.viewport.scrollTop).toBe(450);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(550);
    expect(fixture.last.getBoundingClientRect().top).toBe(370);
  });
});

/* A sent turn kept near the top: a view 200px tall over a transcript whose
   rows report their own places; the room below the turn is a row the owner
   sizes, counted into the scroll height as the browser would. */
function sentTurnFixture() {
  const fixture = transcriptFixture();
  document.body.append(fixture.viewport);
  fixture.viewport.getBoundingClientRect = () =>
    ({ top: 0, bottom: 200, left: 0, width: 300, height: 200 }) as DOMRect;
  const content = fixture.content;
  content.dataset.transcriptContent = "";
  let contentHeight = 300;
  /* the room the owner gives the last turn's block: the extent reaches
     at least that far below the turn, which sits at 300 */
  const turnRoom = () =>
    parseFloat(fixture.viewport.style.getPropertyValue("--turn-room")) || 0;
  let clientHeight = 200;
  Object.defineProperties(fixture.viewport, {
    scrollHeight: {
      configurable: true,
      get: () =>
        lift + Math.max(contentHeight, turnRoom() ? lastStart + turnRoom() : 0),
    },
    clientHeight: { configurable: true, get: () => clientHeight },
  });
  /* a row by its key, the one every row that stands for the same message
     shares; a turn is marked as one */
  /* how far everything has been pushed down by growth above the rows, and
     by the margin the owner hides content behind */
  let lift = 0;
  const shown = () =>
    lift - fixture.viewport.scrollTop + (parseFloat(content.style.marginTop) || 0);
  const row = (key: string, top: number, bottom: number, turn = false) => {
    const el = document.createElement("div");
    el.dataset.timelineKey = key;
    if (turn) el.dataset.turn = "";
    el.getBoundingClientRect = () =>
      ({
        top: top + shown(),
        bottom: bottom + shown(),
      }) as DOMRect;
    return el;
  };
  content.append(row("above", 0, 300, true));
  const blocks: HTMLElement[] = [];
  const starts: number[] = [];
  let lastStart = 300;
  /* the reply below the turn, as one row whose foot is the content's end */
  let reply: HTMLElement | null = null;
  const drawReply = () => {
    if (reply) return;
    reply = document.createElement("div");
    reply.dataset.timelineKey = "reply";
    reply.getBoundingClientRect = () =>
      ({
        top: 360 - fixture.viewport.scrollTop,
        bottom: contentHeight - fixture.viewport.scrollTop,
      }) as DOMRect;
    /* the reply belongs to the last turn's block */
    (blocks[blocks.length - 1] ?? content).append(reply);
  };
  return {
    ...fixture,
    turnRoom,
    /* the person's turn lands under what is there */
    async sendTurn(height = 60, key = "sent", at = 300) {
      const turn = row(key, at, at + height, true);
      /* the turn's block: as tall as its content, or as the room while it
         is the last; the block before it gives its room up */
      const block = document.createElement("div");
      block.setAttribute("data-turn-block", "");
      const start = at;
      block.getBoundingClientRect = () => {
        const index = blocks.indexOf(block);
        const last = index === blocks.length - 1;
        const end = last
          ? Math.max(contentHeight, start + turnRoom())
          : starts[index + 1]!;
        return {
          top: start + shown(),
          bottom: end + shown(),
          height: end - start,
        } as DOMRect;
      };
      await act(async () => {
        blocks.push(block);
        starts.push(start);
        lastStart = at;
        block.append(turn);
        content.append(block);
        contentHeight = at + height;
      });
      return turn;
    },
    /* the content is this tall from now on, with nothing reported yet:
       the change reaches the owner by whatever mutation follows */
    setContent(height: number) {
      contentHeight = height;
    },
    /* the reply below the turn grows to this content height */
    replyTo(height: number) {
      contentHeight = height;
      drawReply();
      fixture.growTo(height);
    },
    /* the bridge's own row replaces the optimistic one, at the same place */
    async replaceTurn(turn: Element, height = 60, key = "sent") {
      const saved = row(key, 300, 300 + height, true);
      await act(async () => {
        turn.replaceWith(saved);
        contentHeight = 300 + height;
      });
      return saved;
    },
    /* the person's turn lands above a reply still streaming: queued
       behind it, drawn before it */
    async sendBehind(tail: Element, height = 60) {
      const turn = row("sent", 300, 300 + height, true);
      await act(async () => {
        content.insertBefore(turn, tail);
        contentHeight += height;
      });
      return turn;
    },
    /* the send was refused, or its row is away for a commit: the row and
       its block leave the transcript, and the block before is the last */
    async refuse(turn: Element) {
      await act(async () => {
        const block = turn.closest("[data-turn-block]");
        const index = block ? blocks.indexOf(block as HTMLElement) : -1;
        if (index >= 0) {
          blocks.splice(index, 1);
          starts.splice(index, 1);
          block!.remove();
        } else turn.remove();
        lastStart = starts[starts.length - 1] ?? 300;
        contentHeight = index > 0 ? starts[index - 1]! + 60 : 300;
      });
    },
    /* a row above everything grows: all of it moves down by as much */
    growAbove(by: number) {
      lift += by;
      fixture.growTo(lift + contentHeight);
    },
    /* the window shrinks to this height */
    shrinkTo(height: number) {
      clientHeight = height;
      fixture.viewport.getBoundingClientRect = () =>
        ({ top: 0, bottom: height, left: 0, width: 300, height }) as DOMRect;
      fixture.growTo(contentHeight);
    },
    /* the turn takes a tool step: it is doing work, not only answering */
    addWork() {
      const step = document.createElement("div");
      step.setAttribute("data-step-key", "s1");
      drawReply();
      step.getBoundingClientRect = () =>
        ({
          top: contentHeight - fixture.viewport.scrollTop,
          bottom: contentHeight - fixture.viewport.scrollTop,
        }) as DOMRect;
      /* work of the last turn: a sibling of its row in its block */
      (blocks[blocks.length - 1] ?? content).append(step);
    },
  };
}

/* the hook as the session mounts it: told the key of a turn sent from here,
   in the commit that draws the app's own copy of it */
function mount(
  fixture: ReturnType<typeof sentTurnFixture>,
  options: { anchorSentTurn?: boolean; sentTurn?: string | null } = {
    anchorSentTurn: true,
  },
) {
  const hook = renderHook(
    (props: typeof options) => useFollowTail(fixture.viewport, "session-1", props),
    { initialProps: options },
  );
  return {
    ...hook,
    tell: (key: string | null) => hook.rerender({ ...options, sentTurn: key }),
    async send(height = 60, key = "sent", at = 300) {
      hook.rerender({ ...options, sentTurn: key });
      return fixture.sendTurn(height, key, at);
    },
  };
}

describe("a sent turn kept near the top of the view", () => {
  beforeEach(() =>
    vi.useFakeTimers({
      toFake: [
        "setTimeout",
        "clearTimeout",
        "performance",
        "requestAnimationFrame",
        "cancelAnimationFrame",
      ],
    }),
  );
  afterEach(() => {
    vi.useRealTimers();
    document.body.replaceChildren();
  });

  it("gives the turn room, glides it to its place, and keeps a plain answer held once it has filled the room", async () => {
    const fixture = sentTurnFixture();
    const { result, send } = mount(fixture);
    expect(fixture.viewport.scrollTop).toBe(100);
    const turn = await send();
    /* 200 of view less the 8px inset and the 32px peek: the room is one
       number, set as the turn is held, before the glide begins */
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(result.current.atBottom).toBe(false);
    /* the glide: part of the way after a few frames, there after it ends;
       a chunk landing mid-glide changes nothing about the room */
    await act(async () => vi.advanceTimersByTimeAsync(100));
    const partWay = fixture.viewport.scrollTop;
    expect(partWay).toBeGreaterThan(100);
    expect(partWay).toBeLessThan(260);
    fixture.replyTo(370);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(turn.getBoundingClientRect().top).toBe(40);

    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(160);
    expect(turn.getBoundingClientRect().top).toBe(40);
    expect(fixture.viewport.dataset.following).toBe("false");

    /* the answer reaches past the room: still held, however long it grows */
    fixture.replyTo(480);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
    expect(result.current.atBottom).toBe(false);
    fixture.replyTo(900);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* a turn doing work is followed from the layout where its reply crosses
     the end of its room */
  it("follows a working turn once it reaches past its room, and not before", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    fixture.addWork();
    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
    fixture.replyTo(480);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.dataset.following).toBe("true");
    expect(fixture.viewport.scrollTop).toBe(280);
  });

  /* back down to the foot while room is left: the turn is held again */
  it("holds the turn again when the reader comes back to the end of its room", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(260);
    readerScrollsTo(fixture.viewport, 100);
    expect(fixture.viewport.dataset.following).toBe("false");
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: 40 }));
      fixture.viewport.scrollTop = 260;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(fixture.viewport.dataset.following).toBe("false");
    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* a turn taller than the view is held with its end in view: its foot no
     lower than the view's, so the reply below it shows */
  it("holds a tall turn partly scrolled off, its end and the reply in view", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send(300);
    expect(fixture.turnRoom()).toBe(300);
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(400);
    expect(turn.getBoundingClientRect().top).toBe(-100);
    expect(turn.getBoundingClientRect().bottom).toBe(200);
    expect(fixture.viewport.dataset.following).toBe("false");
    fixture.replyTo(900);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(-100);
  });

  /* without the preference a send still goes to the turn: the view follows
     the foot its reply will grow, even for a reader who had scrolled up */
  it("follows the foot from a send without the preference, wherever the reader was", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture, { anchorSentTurn: false });
    readerScrollsTo(fixture.viewport, 20);
    expect(fixture.viewport.dataset.following).toBe("false");
    await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.turnRoom()).toBe(0);
    expect(fixture.viewport.scrollTop).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("true");
  });

  /* a reader who had scrolled up sends: the view goes to their message */
  it("holds a sent turn for a reader who was not following", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    readerScrollsTo(fixture.viewport, 20);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(fixture.turnRoom()).toBe(160);
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* a row that is not from a send here (another client's message, an
     older page) moves nothing */
  it("leaves a turn alone that was not sent from here", async () => {
    const fixture = sentTurnFixture();
    mount(fixture);
    await fixture.sendTurn(60, "theirs");
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.turnRoom()).toBe(0);
    expect(fixture.viewport.scrollTop).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("true");
  });

  /* the send is known before its row is drawn: the turn is held when the
     row lands, not the row that was last at the time */
  it("waits for the sent row when the send is known first", async () => {
    const fixture = sentTurnFixture();
    const { tell } = mount(fixture);
    tell("sent");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(fixture.viewport.dataset.following).toBe("true");
    expect(fixture.turnRoom()).toBe(0);
    const turn = await fixture.sendTurn();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* the same send offered again (a re-render, a later read) is not taken
     twice: a working turn being followed is not pinned back */
  it("goes to a send once, however often it is offered", async () => {
    const fixture = sentTurnFixture();
    const { send, tell, result } = mount(fixture);
    await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    act(() => result.current.toBottom());
    expect(fixture.viewport.dataset.following).toBe("true");
    tell(null);
    tell("sent");
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(fixture.viewport.dataset.following).toBe("true");
  });

  /* a second send while the first still shows the app's own copy: the
     view goes to the second, by its own key */
  it("holds the second of two sends, not the first still being saved", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    await send(60, "first");
    await act(async () => vi.advanceTimersByTimeAsync(600));
    const second = await send(60, "second");
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(second.getBoundingClientRect().top).toBe(40);
  });

  /* a turn whose reply has already begun under it (a message that waited
     in the queue, drawn again as its turn starts): held all the same */
  it("holds a sent turn whose reply is already drawn after it", async () => {
    const fixture = sentTurnFixture();
    const { tell } = mount(fixture);
    fixture.addWork();
    const tail = fixture.content.querySelector("[data-step-key]")!;
    fixture.replyTo(700);
    await act(async () => vi.advanceTimersByTimeAsync(50));
    expect(fixture.viewport.dataset.following).toBe("true");
    tell("sent");
    const turn = await fixture.sendBehind(tail);
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* a send from far up the transcript: the view is put within a view of
     the turn at once and glides the rest, not the whole way */
  it("glides only the last view of the way to a turn sent from far away", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    readerScrollsTo(fixture.viewport, 0);
    await send();
    /* 300 to the turn less 40 of rest is 260, a view is 200: 60 at once */
    expect(fixture.viewport.scrollTop).toBe(60);
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(fixture.viewport.scrollTop).toBeGreaterThan(60);
    expect(fixture.viewport.scrollTop).toBeLessThan(260);
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(260);
  });

  /* at the end of the transcript, a reply that grows shorter for a moment
     is clamped by the browser and the turn moves down by as much: there is
     no more content to scroll to, and hiding content above to put the
     turn back would only be clamped again, each time more, pulling the end
     of the transcript and the composer after it out of view */
  it("does not hide content to hold a turn when a shrink has clamped the view at its end", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    /* the reply has filled the room and gone past it */
    fixture.replyTo(520);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    const max = () => fixture.viewport.scrollHeight - fixture.viewport.clientHeight;
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: 60 }));
      fixture.viewport.scrollTop = max();
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    const atEnd = fixture.viewport.scrollTop;
    for (const height of [490, 480]) {
      fixture.setContent(height);
      await act(async () => {
        fixture.viewport.scrollTop = max();
        fixture.viewport.dispatchEvent(new Event("scroll"));
        fixture.content.append(document.createElement("span"));
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(fixture.content.style.marginTop).toBe("");
      expect(fixture.viewport.scrollTop).toBe(max());
    }
    expect(fixture.viewport.scrollTop).toBeLessThan(atEnd);
    expect(turn.isConnected).toBe(true);
  });

  /* the first message of a new session: its view mounts with the send
     already known, and holds the row as it lands */
  it("holds a send known before the view mounts with its subject", async () => {
    const fixture = sentTurnFixture();
    const turn = await fixture.sendTurn();
    renderHook(() =>
      useFollowTail(fixture.viewport, "session-1", {
        anchorSentTurn: true,
        sentTurn: "sent",
      }),
    );
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(turn.getBoundingClientRect().top).toBe(40);
  });

  /* leaving a session and coming back offers its last send again: the
     view starts at the foot, and does not go to that turn a second time */
  it("does not go to a session's last send again on returning to it", async () => {
    const fixture = sentTurnFixture();
    const hook = renderHook(
      (props: { subject: string; sentTurn: string | null }) =>
        useFollowTail(fixture.viewport, props.subject, {
          anchorSentTurn: true,
          sentTurn: props.sentTurn,
        }),
      { initialProps: { subject: "session-1", sentTurn: null as string | null } },
    );
    hook.rerender({ subject: "session-1", sentTurn: "sent" });
    await fixture.sendTurn();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("false");
    hook.rerender({ subject: "session-2", sentTurn: null });
    hook.rerender({ subject: "session-1", sentTurn: "sent" });
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.dataset.following).toBe("true");
    expect(fixture.turnRoom()).toBe(0);
  });

  /* a refused send takes its row away: the room goes with it, and the
     view stays where it is, held by the row under the reading line; the
     browser's clamp onto the shorter content is not undone */
  it("keeps the view still when a refused send removes the turn", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.turnRoom()).toBe(160);
    await fixture.refuse(turn);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(fixture.viewport.scrollTop).toBe(260);
  });

  /* the sent row leaves in one commit and the message is drawn again
     under another key in a later one: the hold moves to the new row when
     it lands, the room having stayed, and nothing moves in between */
  it("moves the hold to a row drawn again later under another key", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    await fixture.refuse(turn);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(fixture.turnRoom()).toBe(160);
    const saved = await fixture.sendTurn(60, "renamed");
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(saved.getBoundingClientRect().top).toBe(40);
    /* and the reply below it keeps it in place, as a held turn's does */
    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(saved.getBoundingClientRect().top).toBe(40);
    /* the reader comes down to the end of the room: held there again */
    readerScrollsTo(fixture.viewport, 100);
    act(() => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: 40 }));
      fixture.viewport.scrollTop = 260;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    fixture.replyTo(430);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(saved.getBoundingClientRect().top).toBe(40);
  });

  /* the sent row gives way to one under another key, the runtime having
     named the request anew: the hold and the room move to the new row */
  it("keeps the hold when the sent row is replaced under another key", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    const saved = await fixture.replaceTurn(turn, 60, "renamed");
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(saved.getBoundingClientRect().top).toBe(40);
    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(saved.getBoundingClientRect().top).toBe(40);
  });

  /* the row standing for a held turn leaves and comes back a moment later
     (the transcript drawing the message as something else for a while):
     the view does not move, and the message is not gone to twice */
  it("stays put when the held turn's row leaves and comes back", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    const before = fixture.viewport.scrollTop;
    await fixture.refuse(turn);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.viewport.scrollTop).toBe(before);
    expect(fixture.viewport.dataset.following).toBe("false");
    await fixture.sendTurn();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(before);
    expect(fixture.viewport.dataset.following).toBe("false");
  });

  /* the bridge's pending row for the send leaves a commit before its saved
     message lands: the block before takes the room, made larger by the
     lost block's start less its own, so the extent and the view stay;
     the row back, the turn is where it was and the room is its own */
  it("keeps the extent and the view while the held turn's row is away", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    await send(60, "first");
    await act(async () => vi.advanceTimersByTimeAsync(600));
    fixture.replyTo(320);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    const second = await send(60, "second", 320);
    await act(async () => vi.advanceTimersByTimeAsync(700));
    expect(fixture.viewport.scrollTop).toBe(280);
    expect(fixture.viewport.scrollHeight).toBe(480);
    expect(fixture.turnRoom()).toBe(160);
    await fixture.refuse(second);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(180);
    expect(fixture.viewport.scrollHeight).toBe(480);
    expect(fixture.viewport.scrollTop).toBe(280);
    const back = await fixture.sendTurn(60, "second", 320);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.scrollTop).toBe(280);
    expect(back.getBoundingClientRect().top).toBe(40);
    expect(fixture.viewport.dataset.following).toBe("false");
  });

  /* the optimistic row gives way to the bridge's own copy, a new element
     under the same key: the hold and the room stay with the turn */
  it("keeps the hold when the sent row is replaced by its saved copy", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(260);
    const saved = await fixture.replaceTurn(turn);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(saved.getBoundingClientRect().top).toBe(40);
    fixture.replyTo(420);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(saved.getBoundingClientRect().top).toBe(40);
  });

  /* the saved copy is taller than the optimistic row: the room is for the
     turn's rest, which does not change for a row of another height */
  it("keeps the room after a taller replacement", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    const saved = await fixture.replaceTurn(turn, 100);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(saved.getBoundingClientRect().top).toBe(40);
    expect(fixture.turnRoom()).toBe(160);
    expect(fixture.viewport.dataset.following).toBe("false");
  });

  /* the composer takes the foot of the view: its room is scroll extent
     after the last turn's block, so the block is that much shorter, and
     the end of the room is still the turn's rest */
  it("leaves the composer's room out of the turn's, as it was when the turn was held", async () => {
    const fixture = sentTurnFixture();
    fixture.viewport.style.setProperty("--composer-h", "50px");
    const { send } = mount(fixture);
    await send();
    expect(fixture.turnRoom()).toBe(110);
    /* the composer grows with what is typed: the room does not follow */
    fixture.viewport.style.setProperty("--composer-h", "120px");
    fixture.replyTo(380);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.turnRoom()).toBe(110);
  });

  /* the composer shrinks after the send (the text it held is gone, a strip
     shown while the reply ran is gone): the room grows by as much, so the
     turn can still reach its place; it never shrinks for the composer */
  it("gives the turn the room the composer frees after the send", async () => {
    const fixture = sentTurnFixture();
    fixture.viewport.style.setProperty("--composer-h", "120px");
    const { send } = mount(fixture);
    await send();
    expect(fixture.turnRoom()).toBe(40);
    fixture.viewport.style.setProperty("--composer-h", "50px");
    fixture.replyTo(380);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.turnRoom()).toBe(110);
    fixture.viewport.style.setProperty("--composer-h", "120px");
    fixture.replyTo(390);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.turnRoom()).toBe(110);
  });

  /* the turn before gave most of its room up when the new one was drawn,
     its reply having been short: the room was below that reply, so nothing
     in view moves, and the new turn glides the short way from just under
     the reply to its place */
  it("keeps the view still as the previous turn's empty room collapses under a new send", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    fixture.replyTo(320);
    await act(async () => vi.advanceTimersByTimeAsync(40));
    expect(fixture.viewport.scrollTop).toBe(260);
    expect(fixture.viewport.scrollHeight).toBe(460);
    /* the next send: the first turn's block is 20 tall now, not 160 */
    const second = await send(60, "second", 320);
    /* 20 short of its place: too little for a glide, put there at once */
    expect(fixture.viewport.scrollTop).toBe(280);
    expect(fixture.viewport.scrollHeight).toBe(480);
    await act(async () => vi.advanceTimersByTimeAsync(700));
    expect(fixture.viewport.scrollTop).toBe(280);
    expect(second.getBoundingClientRect().top).toBe(40);
  });

  /* with motion reduced the turn is put at its place in one write */
  it("puts the turn at its place at once when motion is reduced", async () => {
    const fixture = sentTurnFixture();
    const media = window.matchMedia;
    window.matchMedia = ((query: string) =>
      ({
        matches: query.includes("reduce"),
        media: query,
        addEventListener() {},
        removeEventListener() {},
      }) as unknown as MediaQueryList) as typeof window.matchMedia;
    try {
      const { send } = mount(fixture);
      const turn = await send();
      expect(fixture.viewport.scrollTop).toBe(260);
      expect(turn.getBoundingClientRect().top).toBe(40);
      expect(fixture.viewport.dataset.following).toBe("false");
    } finally {
      window.matchMedia = media;
    }
  });

  /* a correction the owner was still hiding for the turn held before (a
     row above it grew while the view was moving) is put back as the new
     turn is taken, and the glide runs: it does not land as one jump at the
     correction's deadline */
  it("glides to a new send while a correction for the last one is still hidden", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    await send(60, "first");
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.viewport.scrollTop).toBe(260);
    act(() => fixture.viewport.dispatchEvent(new Event("scroll")));
    fixture.growAbove(10);
    expect(fixture.content.style.marginTop).toBe("-10px");
    expect(fixture.viewport.scrollTop).toBe(260);

    const turn = await send(60, "second", 360);
    expect(fixture.content.style.marginTop).toBe("");
    /* the hidden 10 are put back as the turn is taken; the glide starts
       from there */
    expect(fixture.viewport.scrollTop).toBe(270);
    await act(async () => vi.advanceTimersByTimeAsync(100));
    const partWay = fixture.viewport.scrollTop;
    expect(partWay).toBeGreaterThan(270);
    expect(partWay).toBeLessThan(330);
    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(fixture.viewport.scrollTop).toBe(330);
    expect(turn.getBoundingClientRect().top).toBe(40);
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(fixture.viewport.scrollTop).toBe(330);
    expect(fixture.content.style.marginTop).toBe("");
  });

  /* a shrinking window keeps the turn where it is; the room follows the
     view's size, the one thing it follows */
  it("keeps the turn in place and resizes the room with the window", async () => {
    const fixture = sentTurnFixture();
    const { send } = mount(fixture);
    const turn = await send();
    await act(async () => vi.advanceTimersByTimeAsync(600));
    expect(fixture.turnRoom()).toBe(160);
    fixture.shrinkTo(150);
    await act(async () => vi.advanceTimersByTimeAsync(20));
    expect(fixture.turnRoom()).toBe(110);
    expect(turn.getBoundingClientRect().top).toBe(40);
    expect(fixture.viewport.dataset.following).toBe("false");
  });
});

describe("older transcript pages", () => {
  beforeEach(() =>
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] }),
  );
  afterEach(() => vi.useRealTimers());

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
      await act(async () => vi.advanceTimersByTimeAsync(2500));
      expect(fixture.viewport.scrollTop).toBe(500);
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
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(900);
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
        await vi.advanceTimersByTimeAsync(50);
      });
    }
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(500);
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
    await act(async () => vi.advanceTimersByTimeAsync(300));
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    act(() => {
      touch("touchend", []);
    });
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(500);
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
      await vi.advanceTimersByTimeAsync(400);
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(500);
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
      await vi.advanceTimersByTimeAsync(300);
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(300);
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(700);
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
      await vi.advanceTimersByTimeAsync(1100);
    });
    expect(fixture.content.style.marginTop).toBe("-400px");
    expect(fixture.viewport.scrollTop).toBe(100);
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(500);
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
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.content.style.marginTop).toBe("");
    expect(fixture.viewport.scrollTop).toBe(300);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
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
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(501);
    expect(fixture.content.style.marginTop).toBe("");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1200);
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
    await act(async () => vi.advanceTimersByTimeAsync(2500));
    expect(fixture.viewport.scrollTop).toBe(700);
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
