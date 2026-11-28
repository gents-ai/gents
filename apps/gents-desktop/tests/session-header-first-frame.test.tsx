import { act, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useHeaderScrolledOut } from "../src/ui/screens/sessionLayout";

type Report = (entries: { isIntersecting: boolean }[]) => void;
let report: Report | null = null;

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(callback: Report) {
        report = callback;
      }
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  report = null;
});

function Header() {
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const [out, marker] = useHeaderScrolledOut(scroller);
  return (
    <div ref={setScroller}>
      <div ref={marker} />
      <p data-testid="out">{String(out)}</p>
    </div>
  );
}

describe("whether a session's header has scrolled out", () => {
  it("is known from the first frame of a session opened below its header", () => {
    render(<Header />);
    act(() => report!([{ isIntersecting: false }]));
    expect(screen.getByTestId("out")).toHaveTextContent("true");
  });

  it("still waits for the open to settle before any other change", () => {
    render(<Header />);
    act(() => report!([{ isIntersecting: true }]));
    act(() => report!([{ isIntersecting: false }]));
    expect(screen.getByTestId("out")).toHaveTextContent("false");
    act(() => vi.advanceTimersByTime(400));
    expect(screen.getByTestId("out")).toHaveTextContent("true");
  });
});
