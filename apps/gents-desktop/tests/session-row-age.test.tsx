import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Age, when } from "../src/ui/screens/time";

describe("an age on screen", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-03T12:00:00Z"));
  });
  afterEach(() => vi.useRealTimers());

  it("advances with the clock while nothing else re-renders it", () => {
    render(<Age iso="2026-10-03T11:56:00Z" />);
    expect(screen.getByText("4m")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(2 * 60_000);
    });
    expect(screen.getByText("6m")).toBeInTheDocument();
  });

  it("reads as when() does", () => {
    expect(when("2026-10-03T11:59:50Z")).toBe("now");
    expect(when("2026-10-03T09:00:00Z")).toBe("3h");
    expect(when(null)).toBe("");
  });
});
