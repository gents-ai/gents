import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { MessageList } from "../src/components/Transcript";
import type { RenderedTimelineItem } from "../src/lib/types";

describe("Transcript cancel cause surfacing", () => {
  it("renders CancelCauseBadge on cancelled tool calls", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "tools-1",
        messageSequence: 1,
        tools: [
          {
            itemKey: "tool-1",
            toolName: "background_tool",
            statusKind: "error",
            status: "cancelled",
            args: null,
            result: null,
            cancelCause: {
              cause: "userCancelled",
              source: "requestInterrupt",
              confidence: "direct",
              at: "2026-05-20T10:32:14Z",
              evidence: ["AgentRequest.interrupt_requested_at = 2026-05-20T10:32:14Z"],
            },
          },
        ],
      },
    ];
    render(<MessageList timelineItems={items} />);
    expect(screen.getByText(/user cancelled/i)).toBeInTheDocument();
    // Open the disclosure to verify CancelCauseDetails is mounted inside the body.
    // Note: the badge is in summary which is always visible; details are in the body
    // and only rendered when <details> is open. Native <details> defaults to closed,
    // so details content is still in the DOM in jsdom — assert against it directly.
    expect(screen.getByText(/AgentRequest.interrupt_requested_at/)).toBeInTheDocument();
  });

  it("does not render a badge when cancelCause is missing", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "tools-2",
        messageSequence: 2,
        tools: [
          {
            itemKey: "tool-2",
            toolName: "read_file",
            statusKind: "success",
            status: "completed",
            args: null,
            result: null,
            // cancelCause omitted
          },
        ],
      },
    ];
    render(<MessageList timelineItems={items} />);
    expect(screen.queryByText(/user cancelled/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/cause unknown/i)).not.toBeInTheDocument();
  });

  it("renders different cause variants with their own class", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "tools-3",
        messageSequence: 3,
        tools: [
          {
            itemKey: "tool-3",
            toolName: "index_repo",
            statusKind: "error",
            status: "cancelled",
            args: null,
            result: null,
            cancelCause: {
              cause: "deadline",
              source: "toolLifecycle",
              confidence: "derived",
              at: "2026-05-20T10:35:02Z",
              evidence: ["AgentToolCall.lifecycle_state = \"timedOut\""],
            },
          },
        ],
      },
    ];
    render(<MessageList timelineItems={items} />);
    const badge = screen.getByText(/deadline expired/i);
    expect(badge).toHaveClass("cause-deadline");
  });
});
