import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MessageList } from "../src/components/Transcript";
import type { RenderedTimelineItem } from "../src/lib/types";

describe("transcript message times", () => {
  it("renders a dated time label with the raw timestamp on hover", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "userMessage",
        itemKey: "m1",
        sequence: 1,
        content: "hello",
        timestamp: "2026-06-03T14:05:00Z",
      },
      {
        kind: "assistantMessage",
        itemKey: "m2",
        sequence: 2,
        content: "hi",
        reasoning: null,
        timestamp: "2026-06-03T14:06:00Z",
      },
    ];
    render(<MessageList timelineItems={items} />);

    const times = screen.getAllByTitle(/2026-06-03T14:0/);
    expect(times).toHaveLength(2);
    expect(times[0].tagName).toBe("TIME");
    expect(times[0]).toHaveTextContent(/Jun 3/);
  });

  it("omits the label when the timestamp is missing or unparsable", () => {
    const items: RenderedTimelineItem[] = [
      { kind: "userMessage", itemKey: "m1", sequence: 1, content: "hello" },
      {
        kind: "assistantMessage",
        itemKey: "m2",
        sequence: 2,
        content: "hi",
        reasoning: null,
        timestamp: "not-a-date",
      },
    ];
    const { container } = render(<MessageList timelineItems={items} />);
    expect(container.querySelector("time")).toBeNull();
  });
});
