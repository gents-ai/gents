import { render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatTranscriptPanel } from "../src/components/chat/ChatTranscriptPanel";
import type { DesktopSessionSnapshot } from "../src/lib/types";

function sessionWithLiveContent(content: string): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:defra:amy",
    behaviorId: "default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "streaming",
    latestRequestId: "req-1",
    latestResponse: null,
    activeResponseOverlay: null,
    pendingTurn: null,
    timelineItems: [
      {
        kind: "liveAssistant",
        itemKey: "live:req-1",
        content,
        reasoning: null,
      },
    ],
  };
}

describe("ChatTranscriptPanel", () => {
  it("continues following the live assistant tail as content grows", async () => {
    const scrollIntoView = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(() => {});

    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="session-1"
        session={sessionWithLiveContent("hello")}
      />,
    );

    await waitFor(() => {
      expect(scrollIntoView).toHaveBeenCalled();
    });
    scrollIntoView.mockClear();

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="session-1"
        session={sessionWithLiveContent("hello with more streamed text")}
      />,
    );

    await waitFor(() => {
      expect(scrollIntoView).toHaveBeenCalled();
    });
    scrollIntoView.mockRestore();
  });
});
