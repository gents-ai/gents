import { act, fireEvent, renderHook, waitFor } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { useFollowTail, useOlderPages } from "../src/ui/lib/scroll";

/* jsdom has no ResizeObserver: these stand in for the content box growing */
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
    /* the content box grows, by a new row or by text revealed in place */
    growTo(height: number) {
      scrollHeight = height;
      act(() => observers.forEach((notify) => notify()));
    },
    setHeight(height: number) {
      scrollHeight = height;
    },
  };
}

/* the reader's own input, then the scroll it makes */
function readerScrollsTo(viewport: HTMLElement, top: number) {
  act(() => {
    viewport.dispatchEvent(new Event("wheel"));
    viewport.scrollTop = top;
    viewport.dispatchEvent(new Event("scroll"));
  });
}

/* the foot: the end of the content at the bottom of the 200px view */
const foot = (height: number) => height - 200;

describe("transcript streaming follow", () => {
  it("stays pinned across growth, releases on the reader's scroll up, and relocks near the foot", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));

    expect(fixture.viewport.scrollTop).toBe(foot(500));
    expect(result.current.atBottom).toBe(true);

    // A chunk larger than the view arrives: following is the reader's
    // standing choice, not a judgment of the new height.
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

  /* WebKit eases a held arrow key in a few pixels at a time, so its first
     frames are still within reach of the foot */
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

    // the browser clamps or resets the position: no wheel, touch or key
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
    // WebKit clamps partway through an update that replaces a row; the
    // content ends at the same height, so no resize is reported
    fixture.viewport.scrollTop = foot(500) - 72;
    await act(async () => {
      fixture.viewport.firstElementChild!.append(document.createElement("p"));
    });
    expect(fixture.viewport.scrollTop).toBe(foot(500));
  });

  it("leaves a scroll the reader did not make where it lands", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));
    // find-in-page or focus moving into the transcript: no wheel, touch or
    // key, and the view is not dragged back to the foot
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

    /* the jump's own scroll event lands inside the wheel's intent window */
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

describe("older transcript pages", () => {
  /* the load queues the older rows as React state; they reach the screen in
     a commit before the load resolves (a store's synchronous update) or with
     the commit that ends loading (a batched one). The row is put back in that
     commit either way: the reader moved just now, so by pulling the content
     up over the new rows, and once the view is still, by the position. */
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
      await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500));
      expect(content.style.marginTop).toBe("");
      /* still within three views of the top and moving up: the next page
         follows without another scroll */
      await waitFor(() => expect(load).toHaveBeenCalledTimes(2));
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
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(900));
    expect(load).toHaveBeenCalledTimes(1);
    expect(result.current).toBe(false);
    unmount();
  });

  /* a trackpad's momentum goes on sending wheel events after it is too slow
     to move the view; the rows stay hidden until those stop too */
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
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500));
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  /* a finger held still on a touchscreen is still scrolling */
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
    await waitFor(() => expect(fixture.viewport.scrollTop).toBe(500));
    expect(fixture.content.style.marginTop).toBe("");
    unmount();
  });

  /* at the top the view has stopped against the end, so the position can be
     written while the reader is still scrolling */
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

  /* the page was asked for in the session the reader left: neither its
     landing nor its hidden rows reach the next session's content */
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
     one commit; the next page must still be asked for */
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
