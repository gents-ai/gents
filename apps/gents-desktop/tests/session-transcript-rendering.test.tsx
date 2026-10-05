import { act, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

const markdownRender = vi.hoisted(() => vi.fn());

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => {
    markdownRender(children);
    return <div>{children}</div>;
  },
}));

import { TranscriptPanel } from "../src/ui/screens/SessionScreen";

function session(content: string): DesktopSessionSnapshot {
  return {
    sessionId: "session-long",
    agentDid: "did:test:agent",
    behaviorId: "behavior-default",
    title: "Long session",
    previewText: content,
    status: "completed",
    turnState: "completed",
    latestRequestId: "request-1",
    retryEligibility: { eligible: false, denialReason: null },
    latestRequestOutcome: null,
    pendingTurn: null,
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 1,
      compactionThreshold: 0,
      compactionThresholdTokens: 0,
      durableMessageCount: 1,
      providerMessageCount: 1,
      totalCompactedMessages: 0,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: [
      {
        kind: "assistantMessage",
        itemKey: "assistant-1",
        sequence: 1,
        content,
        reasoning: null,
        timestamp: null,
        reconstruction: { state: "ready" },
      },
    ],
  };
}

describe("SessionScreen transcript render boundary", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("renders failures and retries through the latest action owner, then shows interruption", async () => {
    const failed = {
      ...session("partial response"),
      turnState: "failed",
      retryEligibility: { eligible: true, denialReason: null },
    };
    const oldRetry = vi.fn(async () => null);
    const latestRetry = vi.fn(async () => null);
    const actionsRef = {
      current: {
        loadOlderSessionTimeline: vi.fn(async () => false),
        retryMessage: oldRetry,
      },
    };
    const props = {
      actionsRef,
      holdsCount: 0,
      inFlight: false,
      scroller: null,
    };
    const view = render(<TranscriptPanel {...props} session={failed} />);
    expect(screen.getByText("The assistant could not finish this turn.")).toBeVisible();
    actionsRef.current = { ...actionsRef.current, retryMessage: latestRetry };
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Retry" })),
    );
    expect(latestRetry).toHaveBeenCalledWith("request-1");
    expect(oldRetry).not.toHaveBeenCalled();
    view.rerender(
      <TranscriptPanel {...props} session={{ ...failed, turnState: "interrupted" }} />,
    );
    expect(screen.getByText("This response was stopped.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
  });

  it("keeps unchanged rows out of unrelated session projection renders", () => {
    markdownRender.mockClear();
    const original = session("stable markdown");
    const actionsRef = {
      current: {
        loadOlderSessionTimeline: vi.fn(async () => false),
        retryMessage: vi.fn(async () => null),
      },
    };
    const scroller = null;
    const props = {
      actionsRef,
      holdsCount: 0,
      inFlight: false,
      scroller,
    };
    const view = render(<TranscriptPanel {...props} session={original} />);

    expect(markdownRender).toHaveBeenCalledTimes(1);

    view.rerender(
      <TranscriptPanel
        {...props}
        session={{ ...original, previewText: "projection-only update" }}
      />,
    );
    expect(markdownRender).toHaveBeenCalledTimes(1);

    view.rerender(<TranscriptPanel {...props} session={session("changed markdown")} />);
    expect(markdownRender).toHaveBeenCalledTimes(2);
  });

  it.each([true, false])(
    "adjusts reading position only when an older page adds rows (%s)",
    async (loaded) => {
      let scrollHeight = 100;
      const viewport = document.createElement("div");
      Object.defineProperty(viewport, "scrollHeight", {
        configurable: true,
        get: () => scrollHeight,
      });
      viewport.scrollTop = 20;
      const original = session("stable markdown");
      original.timelinePage = {
        totalItems: 41,
        pageItems: 1,
        hasOlder: true,
        hasNewer: false,
        oldestItemKey: "assistant-1",
        newestItemKey: "assistant-1",
      };
      const older = session("older markdown").timelineItems[0]!;
      const withOlder: DesktopSessionSnapshot = {
        ...original,
        timelineItems: [
          { ...older, itemKey: "assistant-0", sequence: 0 },
          ...original.timelineItems,
        ],
      };
      /* as the session owner does: the older page is set as state, then the
         load reports whether it added rows */
      function Owner() {
        const [current, setCurrent] = useState(original);
        const loadOlderSessionTimeline = async () => {
          if (loaded) {
            scrollHeight = 180;
            setCurrent(withOlder);
          }
          return loaded;
        };
        return (
          <TranscriptPanel
            actionsRef={{
              current: {
                loadOlderSessionTimeline,
                retryMessage: vi.fn(async () => null),
              },
            }}
            holdsCount={0}
            inFlight={false}
            scroller={viewport}
            session={current}
          />
        );
      }

      render(<Owner />);
      await act(async () => {
        fireEvent.wheel(viewport, { deltaY: -20 });
      });

      expect(viewport.scrollTop).toBe(loaded ? 100 : 20);
    },
  );
});
