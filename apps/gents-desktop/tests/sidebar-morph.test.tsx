import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { sidebar, useSidebarMorph } from "../src/ui/app/sidebarChange";
import { useNavResize } from "../src/ui/app/useNavResize";
import { useMorphCard } from "../src/ui/lib/morph";
import {
  preferences,
  resetPreferences,
  sidebarMode,
  usePreferences,
} from "../src/ui/preferences";

/* the names each transition saw, as it began */
let named: string[][] = [];
type Started = { finished: Promise<unknown>; ready?: Promise<unknown> };
const start = vi.fn((update: () => void): Started => {
  named.push(
    [...document.querySelectorAll<HTMLElement>("[data-testid]")]
      .filter((el) => el.style.getPropertyValue("view-transition-name"))
      .map((el) => el.dataset.testid!),
  );
  update();
  return { finished: Promise.resolve() };
});

function Shell() {
  const shell = useMorphCard();
  useSidebarMorph(shell.morph);
  const nav = usePreferences((p) => p.nav);
  const resize = useNavResize(
    usePreferences((p) => p.navWidth),
    Infinity,
    nav !== "expanded",
  );
  return (
    <>
      <div ref={shell.part("sidebar")} data-testid="sidebar" />
      <span ref={shell.part("icon:sessions")} data-testid="icon" />
      <div {...resize.handleProps} aria-label="Resize sidebar" />
    </>
  );
}

const matchMedia = window.matchMedia;
beforeEach(() => {
  named = [];
  start.mockClear();
  Object.defineProperty(document, "startViewTransition", {
    value: start,
    configurable: true,
  });
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
  HTMLElement.prototype.setPointerCapture ??= () => {};
});
afterEach(() => {
  delete (document as { startViewTransition?: unknown }).startViewTransition;
  window.matchMedia = matchMedia;
  act(() => resetPreferences());
});

describe("the sidebar's mode changes", () => {
  it("go through the shell's morph, its column and icons named", () => {
    render(<Shell />);
    act(() => sidebar.setMode("collapsed"));
    expect(start).toHaveBeenCalledTimes(1);
    expect(named[0]).toEqual(["sidebar", "icon"]);
    expect(sidebarMode()).toBe("collapsed");
  });

  it("move the column alone when the sidebar is hidden or shown", () => {
    render(<Shell />);
    act(() => sidebar.setMode("hidden"));
    act(() => sidebar.setMode("expanded"));
    expect(named).toEqual([["sidebar"], ["sidebar"]]);
  });

  it("still change when the browser skips the transition", async () => {
    start.mockImplementationOnce((update: () => void) => {
      update();
      return {
        finished: Promise.resolve(),
        ready: Promise.reject(new DOMException("Transition was skipped", "AbortError")),
      };
    });
    render(<Shell />);
    act(() => sidebar.setMode("collapsed"));
    await act(async () => {});
    expect(sidebarMode()).toBe("collapsed");
  });

  it("cleans up when a skipped transition rejects after its update", async () => {
    start.mockImplementationOnce((update: () => void) => {
      update();
      return {
        finished: Promise.reject(
          new DOMException("Transition was skipped", "AbortError"),
        ),
        ready: Promise.reject(new DOMException("Transition was skipped", "AbortError")),
      };
    });
    render(<Shell />);
    act(() => sidebar.setMode("collapsed"));
    await act(async () => {});
    expect(sidebarMode()).toBe("collapsed");
    expect(
      screen.getByTestId("sidebar").style.getPropertyValue("view-transition-name"),
    ).toBe("");
  });

  it("are not morphed while the edge is dragged, nor when it lets go", () => {
    act(() => preferences.setNav("expanded"));
    render(<Shell />);
    const handle = screen.getByRole("separator", { name: "Resize sidebar" });
    fireEvent.pointerDown(handle, { button: 0, clientX: 300, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 40, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: 40, pointerId: 1 });
    expect(sidebarMode()).toBe("collapsed");
    expect(start).not.toHaveBeenCalled();
  });
});
