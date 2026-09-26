import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MessageList } from "@source-inc/gents-desktop-chat";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

describe("subagent transcript tool", () => {
  it("renders a running subagent start as an open lifecycle card", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "spawn-group",
        messageSequence: 2,
        tools: [
          {
            itemKey: "spawn-1",
            toolName: "create_session",
            status: "running",
            statusKind: "running",
            awaitMode: "background",
            presentation: {
              kind: "subagent",
              action: "start",
              name: "researcher",
              sessionId: "session-123456789",
              description: "Trace the completion control flow",
              output: null,
            },
            reconstruction: { state: "ready" },
            partialOutputTail: "Reading watcher.rs",
          },
        ],
      },
    ];

    const { container, getAllByText, getByText } = render(
      <MessageList timelineItems={items} />,
    );
    const card = container.querySelector('[data-testid="tool-spawn-1"]');

    expect(card).not.toBeNull();
    expect(card?.hasAttribute("open")).toBe(true);
    expect(getByText("researcher")).toBeTruthy();
    expect(getByText("background")).toBeTruthy();
    expect(getByText("working")).toBeTruthy();
    expect(getAllByText("Trace the completion control flow")).toHaveLength(2);
    expect(getByText("Reading watcher.rs")).toBeTruthy();
  });

  it("renders a terminal message output and status", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "spawn-group-complete",
        messageSequence: 2,
        tools: [
          {
            itemKey: "spawn-complete",
            toolName: "send_message",
            status: "completed",
            statusKind: "success",
            awaitMode: "background",
            presentation: {
              kind: "subagent",
              action: "message",
              name: "reviewer",
              sessionId: "session-complete",
              description: "Review the patch",
              output: "No blocking issues found.",
            },
            reconstruction: { state: "ready" },
          },
        ],
      },
    ];

    const { getByText } = render(<MessageList timelineItems={items} />);

    expect(getByText("completed")).toBeTruthy();
    expect(getByText("background")).toBeTruthy();
    expect(getByText("session-complete")).toBeTruthy();
    expect(getByText("No blocking issues found.")).toBeTruthy();
  });
});
