// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { afterEach, expect, it } from "vitest";
import { TimelineItem } from "./TimelineItem.js";

afterEach(cleanup);
const item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }> = {
  kind: "sessionCompaction",
  itemKey: "compact-1",
  sequence: 3,
  messagesCompacted: 569,
  originalTokens: 24000,
  compactedTokens: 1200,
  timestamp: "2026-11-05T10:00:00Z",
};

it("explains the observed count in collapsed native details without implying deleted history", () => {
  const { container, rerender } = render(<TimelineItem item={item} />);
  const summary = screen.getByText("Session context compacted · 569 messages");
  const details = container.querySelector("details")!;
  expect(details).not.toBeNull();
  expect(details.open).toBe(false);
  fireEvent.click(summary);
  expect(screen.getByText(/Messages summarized for the model/)).toBeTruthy();
  expect(screen.getByText("24,000 → 1,200 tokens")).toBeTruthy();
  expect(screen.getByText("2026-11-05T10:00:00Z")).toBeTruthy();
  details.open = true;
  rerender(<TimelineItem item={{ ...item }} />);
  expect(container.querySelector("details")!.open).toBe(true);
});

it("preserves zero and omits unavailable count, token and time facts", () => {
  const { container, rerender } = render(
    <TimelineItem
      item={{
        ...item,
        messagesCompacted: 0,
        originalTokens: 0,
        compactedTokens: 0,
        timestamp: null,
      }}
    />,
  );
  expect(
    screen.getByText("Session context compacted · 0 messages"),
  ).toBeTruthy();
  expect(screen.getByText("0 → 0 tokens")).toBeTruthy();
  rerender(
    <TimelineItem
      item={{
        ...item,
        messagesCompacted: null,
        originalTokens: null,
        compactedTokens: null,
        timestamp: null,
      }}
    />,
  );
  expect(screen.getByText("Session context compacted")).toBeTruthy();
  expect(container.textContent).not.toContain("tokens");
  expect(container.querySelector("time")).toBeNull();
});

it("does not invent a complete token reduction from a partial observation", () => {
  const { container } = render(
    <TimelineItem item={{ ...item, originalTokens: null }} />,
  );
  expect(container.textContent).not.toContain("tokens");
});
