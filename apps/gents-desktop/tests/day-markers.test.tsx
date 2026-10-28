import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { dayLabel, dayOf, dayOpenings } from "../src/ui/screens/TranscriptMarker";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { renderIn, testApp } from "./app-fixture";
import { assistantMessage, sessionSnapshot, userMessage } from "./timeline-fixture";

const at = (daysAgo: number, hour: number) => {
  const date = new Date();
  date.setDate(date.getDate() - daysAgo);
  date.setHours(hour, 15, 0, 0);
  return date.toISOString();
};

describe("day labels", () => {
  const now = new Date(2026, 9, 27, 15, 0);
  const day = (y: number, m: number, d: number) => new Date(y, m, d).getTime();

  it("names today and yesterday, then the weekday and date", () => {
    expect(dayLabel(day(2026, 9, 27), now)).toBe("Today");
    expect(dayLabel(day(2026, 9, 26), now)).toBe("Yesterday");
    const earlier = dayLabel(day(2026, 9, 20), now);
    expect(earlier).toMatch(/Tuesday/);
    expect(earlier).toMatch(/20/);
    expect(earlier).not.toMatch(/2026/);
  });

  it("adds the year to a day from another year", () => {
    expect(dayLabel(day(2025, 11, 31), now)).toMatch(/2025/);
  });

  it("reads a timestamp's local day, and nothing from a missing or bad one", () => {
    expect(dayOf("2026-10-27T15:00:00")).toBe(day(2026, 9, 27));
    expect(dayOf(null)).toBeNull();
    expect(dayOf("not a time")).toBeNull();
  });

  it("opens a day at its first dated row and skips rows without a time", () => {
    const rows = [
      { key: "a", at: null },
      { key: "b", at: "2026-10-26T09:00:00" },
      { key: "c", at: "2026-10-26T18:00:00" },
      { key: "d", at: null },
      { key: "e", at: "2026-10-27T08:00:00" },
    ];
    const opens = dayOpenings(
      rows,
      (row) => row.key,
      (row) => row.at,
    );
    expect([...opens.keys()]).toEqual(["b", "e"]);
  });
});

describe("day markers in the transcript", () => {
  it("draws one above the first row of each day", () => {
    const session = sessionSnapshot({
      timelineItems: [
        userMessage({
          itemKey: "u1",
          sequence: 1,
          content: "first",
          timestamp: at(1, 16),
        }),
        assistantMessage({
          itemKey: "a2",
          sequence: 2,
          content: "reply",
          timestamp: at(1, 16),
        }),
        userMessage({
          itemKey: "u3",
          sequence: 3,
          content: "next",
          timestamp: at(0, 9),
        }),
      ],
    });
    renderIn(
      testApp(),
      <TranscriptPanel
        inFlight={false}
        scroller={null}
        session={session}
        workers={NO_WORKERS}
        parentWork={NO_PARENT}
        workerActions={{ interrupt: () => {} }}
      />,
    );
    const markers = screen.getAllByTestId("day-marker");
    expect(markers.map((marker) => marker.textContent)).toEqual(["Yesterday", "Today"]);
    expect(
      within(markers[1].closest("[data-timeline-key]")!).getByText("next"),
    ).toBeInTheDocument();
  });
});
