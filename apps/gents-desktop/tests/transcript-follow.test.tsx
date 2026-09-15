import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { RefObject } from "react";

import { useTranscriptFollow } from "../src/ui/screens/SessionScreen";

function transcriptFixture() {
  const owner = document.createElement("div");
  const viewport = document.createElement("div");
  viewport.dataset.slot = "scroll-area-viewport";
  owner.append(viewport);

  let scrollHeight = 500;
  Object.defineProperties(viewport, {
    clientHeight: { configurable: true, get: () => 200 },
    scrollHeight: { configurable: true, get: () => scrollHeight },
  });

  return {
    ownerRef: { current: owner } as RefObject<HTMLDivElement | null>,
    viewport,
    growTo(height: number) {
      scrollHeight = height;
    },
  };
}

describe("transcript streaming follow", () => {
  it("stays pinned across growth, releases on scroll up, and relocks at the tip", () => {
    const fixture = transcriptFixture();
    const { result, rerender } = renderHook(
      ({ signal }) => useTranscriptFollow(fixture.ownerRef, "session-1", signal),
      { initialProps: { signal: "assistant:10" } },
    );

    expect(fixture.viewport.scrollTop).toBe(500);
    expect(result.current.atBottom).toBe(true);

    // The model appends a chunk larger than the proximity threshold. Follow is
    // based on the reader's prior intent, not the newly increased height.
    fixture.growTo(900);
    rerender({ signal: "assistant:410" });
    expect(fixture.viewport.scrollTop).toBe(900);

    act(() => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(false);

    fixture.growTo(1_200);
    rerender({ signal: "assistant:710" });
    expect(fixture.viewport.scrollTop).toBe(100);

    act(() => {
      fixture.viewport.scrollTop = 1_000;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(1_500);
    rerender({ signal: "assistant:1010" });
    expect(fixture.viewport.scrollTop).toBe(1_500);
  });
});
