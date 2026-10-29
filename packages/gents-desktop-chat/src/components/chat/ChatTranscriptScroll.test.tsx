// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, test, expect, vi } from "vitest";
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { ChatTranscriptPanel } from "./ChatTranscriptPanel.js";

const originalScrollTo = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollTo",
);
afterEach(() => {
  cleanup();
  if (originalScrollTo)
    Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
  vi.restoreAllMocks();
});

test.each([false, true])(
  "retains the latest user row when scrolling away from an active long turn (page older: %s)",
  (pageOlder) => {
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(500);
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(320);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(500);
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(320);
    Object.defineProperty(HTMLElement.prototype, "scrollTo", {
      configurable: true,
      value: vi.fn(),
    });
    const timelineItems: RenderedTimelineItem[] = [
      ...Array.from({ length: 20 }, (_, index) => ({
        kind: "assistantMessage" as const,
        itemKey: `older-${index}`,
        content: `older response ${index}`,
        reconstruction: { state: "ready" as const },
      })),
      {
        kind: "userMessage",
        itemKey: "user",
        requestId: "r",
        inputRequestId: "r",
        ownsTurn: true,
        content: "latest user marker",
        timestamp: null,
        reconstruction: { state: "ready" },
      },
      ...Array.from({ length: 100 }, (_, index) => ({
        kind: "assistantMessage" as const,
        itemKey: `a-${index}`,
        content: `response ${index}`,
        reconstruction: { state: "ready" as const },
      })),
    ];
    const session = {
      sessionId: "s",
      nodeDid: "did:x",
      agentId: "a",
      title: null,
      previewText: null,
      status: "active",
      goal: null,
      turnState: "running",
      latestRequestId: null,
      retryEligibility: { eligible: false, denialReason: "notFailed" },
      latestRequestOutcome: null,
      pendingTurn: null,
      queuedTurns: [],
      foldedInputs: [],
      timelineItems,
      context: {
        estimatedDurableTokens: 0,
        estimatedConversationTokens: 0,
        contextWindow: 1,
        compactionThreshold: 1,
        compactionThresholdTokens: 1,
        compactionStrategy: "summary",
        durableMessageCount: 0,
        providerMessageCount: 0,
        totalCompactedMessages: 0,
        sessionCompactionCount: null,
        requestReductionHistoryLoaded: false,
        compactions: [],
        lastRequest: null,
      },
    } satisfies DesktopSessionSnapshot;
    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="s"
        session={session}
        activity={null}
      />,
    );
    const panel = screen.getByTestId("transcript-panel");
    expect(screen.queryByText("latest user marker")).not.toBeNull();
    Object.defineProperties(panel, {
      scrollHeight: { configurable: true, value: 12000 },
      clientHeight: { configurable: true, value: 500 },
      scrollTop: { configurable: true, writable: true, value: 5000 },
    });
    if (pageOlder) {
      panel.scrollTop = 11500;
      fireEvent.scroll(panel);
      panel.scrollTop = 100;
      fireEvent.scroll(panel);
      expect(screen.queryByText("latest user marker")).not.toBeNull();
      expect(screen.getByText("older response 0")).toBeTruthy();
      const olderPage: RenderedTimelineItem[] = Array.from(
        { length: 10 },
        (_, index) => ({
          kind: "assistantMessage" as const,
          itemKey: `remote-older-${index}`,
          content: `remote older response ${index}`,
          reconstruction: { state: "ready" as const },
        }),
      );
      rerender(
        <ChatTranscriptPanel
          selectedSessionId="s"
          session={{
            ...session,
            timelineItems: [...olderPage, ...timelineItems],
          }}
          activity={null}
        />,
      );
      expect(screen.getByText("remote older response 0")).toBeTruthy();
      expect(screen.getByText("latest user marker")).toBeTruthy();
      const nextItem: RenderedTimelineItem = {
        kind: "assistantMessage",
        itemKey: "page-stream",
        content: "response after paging",
        reconstruction: { state: "ready" },
      };
      rerender(
        <ChatTranscriptPanel
          selectedSessionId="s"
          session={{
            ...session,
            timelineItems: [...olderPage, ...timelineItems, nextItem],
          }}
          activity={null}
        />,
      );
      expect(screen.getByText("remote older response 0")).toBeTruthy();
      expect(screen.getByText("older response 0")).toBeTruthy();
      expect(screen.getByText("latest user marker")).toBeTruthy();
      expect(panel.querySelector('li[aria-setsize="132"]')).not.toBeNull();
      return;
    }
    fireEvent.scroll(panel);
    expect(screen.queryByText("latest user marker")).not.toBeNull();
    expect(
      screen.getByText("latest user marker").closest("li")?.className,
    ).toBe("current-user-turn");
    const appended: RenderedTimelineItem = {
      kind: "assistantMessage",
      itemKey: "appended",
      content: "new response",
      reconstruction: { state: "ready" },
    };
    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s"
        session={{ ...session, timelineItems: [...timelineItems, appended] }}
        activity={null}
      />,
    );
    expect(screen.queryByText("latest user marker")).not.toBeNull();
    expect(screen.queryByText("new response")).not.toBeNull();
    panel.scrollTop = 11500;
    fireEvent.scroll(panel);
    expect(screen.queryByText("latest user marker")).not.toBeNull();
    panel.scrollTop = 5000;
    fireEvent.scroll(panel);
    expect(screen.queryByText("latest user marker")).not.toBeNull();

    const streamed = Array.from({ length: 50 }, (_, index) => ({
      kind: "assistantMessage" as const,
      itemKey: `streamed-${index}`,
      content: `streamed response ${index}`,
      reconstruction: { state: "ready" as const },
    }));
    for (let count = 1; count <= streamed.length; count += 1) {
      rerender(
        <ChatTranscriptPanel
          selectedSessionId="s"
          session={{
            ...session,
            timelineItems: [
              ...timelineItems,
              appended,
              ...streamed.slice(0, count),
            ],
          }}
          activity={null}
        />,
      );
      expect(screen.queryByText("latest user marker")).not.toBeNull();
    }
    expect(panel.querySelectorAll("li[data-index]").length).toBeGreaterThan(0);
    expect(panel.querySelectorAll("li[data-index]").length).toBeLessThanOrEqual(
      200,
    );
    const longStream = Array.from({ length: 250 }, (_, index) => ({
      kind: "assistantMessage" as const,
      itemKey: `long-${index}`,
      content: `long response ${index}`,
      reconstruction: { state: "ready" as const },
    }));
    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s"
        session={{
          ...session,
          timelineItems: [...timelineItems, ...longStream],
        }}
        activity={null}
      />,
    );
    const virtualRows = panel.querySelectorAll("li[data-index]");
    expect(virtualRows.length).toBeGreaterThan(0);
    expect(virtualRows.length).toBeLessThan(200);
    expect(virtualRows[0]?.getAttribute("aria-setsize")).toBe("351");

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s"
        session={{
          ...session,
          turnState: "completed",
          timelineItems: [...timelineItems, appended, ...streamed],
        }}
        activity={null}
      />,
    );
    expect(screen.queryByText("latest user marker")).not.toBeNull();
    expect(
      screen.getByText("latest user marker").closest("li")?.className,
    ).not.toBe("current-user-turn");
    rerender(
      <ChatTranscriptPanel
        selectedSessionId="other"
        session={{ ...session, sessionId: "other", turnState: "completed" }}
        activity={null}
      />,
    );
    expect(screen.queryByText("latest user marker")).toBeNull();
    expect(screen.queryByText("response 99")).not.toBeNull();
    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s"
        session={{
          ...session,
          turnState: "completed",
          timelineItems: [...timelineItems, appended, ...streamed],
        }}
        activity={null}
      />,
    );
    panel.scrollTop = 11500;
    fireEvent.scroll(panel);
    expect(screen.queryByText("latest user marker")).toBeNull();
    expect(screen.queryByText("streamed response 49")).not.toBeNull();
  },
);
