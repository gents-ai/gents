import { act, render } from "@testing-library/react";
import { useRef } from "react";
import { describe, expect, it } from "vitest";

import { useFollowNewest, useScrollEdges } from "../src/ui/lib/scroll";

/* a scroll area 100px tall over 500px of rows, as the kit's ScrollArea nests it */
function Box({
  rows,
  paused = false,
  edges,
}: {
  rows: number;
  paused?: boolean;
  edges?: (seen: { above: boolean; below: boolean }) => void;
}) {
  const owner = useRef<HTMLDivElement>(null);
  const seen = useScrollEdges(owner);
  useFollowNewest(owner, { rows, paused, enabled: true });
  edges?.(seen);
  return (
    <div ref={owner}>
      <div data-slot="scroll-area-viewport" data-testid="viewport">
        <div />
      </div>
    </div>
  );
}

function sized(viewport: HTMLElement) {
  Object.defineProperty(viewport, "scrollHeight", { configurable: true, value: 500 });
  Object.defineProperty(viewport, "clientHeight", { configurable: true, value: 100 });
  return viewport;
}

/* sized once mounted; the hooks read the sizes each time they measure */
function renderBox(props: Parameters<typeof Box>[0]) {
  const view = render(<Box {...props} />);
  return { view, viewport: sized(view.getByTestId("viewport")) };
}

const scrollTo = (viewport: HTMLElement, top: number) =>
  act(() => {
    viewport.scrollTop = top;
    viewport.dispatchEvent(new Event("scroll"));
  });

describe("a capped box that follows its newest row", () => {
  it("follows new rows while at its foot, and stays where the reader scrolled", () => {
    const { view, viewport } = renderBox({ rows: 1 });
    view.rerender(<Box rows={2} />);
    expect(viewport.scrollTop).toBe(500);

    scrollTo(viewport, 0);
    view.rerender(<Box rows={3} />);
    expect(viewport.scrollTop).toBe(0);
  });

  it("waits while a step is open, then decides again from where the box is", () => {
    const { view, viewport } = renderBox({ rows: 1 });
    scrollTo(viewport, 0);
    view.rerender(<Box rows={1} paused />);
    /* the reader closes the step at the box's foot */
    viewport.scrollTop = 400;
    view.rerender(<Box rows={1} />);
    expect(viewport.scrollTop).toBe(400);
    view.rerender(<Box rows={2} />);
    expect(viewport.scrollTop).toBe(500);
  });
});

describe("the fades at a box's edges", () => {
  it("says which edges hide rows as the box scrolls", () => {
    let seen = { above: false, below: false };
    const { viewport } = renderBox({ rows: 1, edges: (next) => (seen = next) });
    scrollTo(viewport, 0);
    expect(seen).toEqual({ above: false, below: true });
    scrollTo(viewport, 400);
    expect(seen).toEqual({ above: true, below: false });
  });
});
