import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ChatHeader,
  ChatTranscriptPanel,
  MessageList,
} from "@source-inc/gents-desktop-chat";
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";

function makeSession(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "s1",
    agentDid: "did:test:operator",
    turnState: "streaming",
    timelineItems: [],
    ...overrides,
  };
}

const pendingTurn: RenderedTimelineItem = {
  kind: "pendingUserTurn",
  itemKey: "p1",
  requestId: "req_1",
  content: "do the thing",
  selectedSkillIds: [],
  lifecycleState: "processing",
};

function expectReasoningBeforeAnswer() {
  const assistant = screen.getByTestId("assistant-message");
  const reasoning = assistant.querySelector(".reasoning-disclosure");
  const answer = assistant.querySelector(".message-content");
  expect(reasoning).not.toBeNull();
  expect(reasoning?.nextElementSibling).toBe(answer);
}

describe("session context visibility", () => {
  it("shows current provider-view pressure, threshold, and compaction history", () => {
    render(
      <ChatHeader
        behaviorLabel="mobile"
        context={{
          estimatedDurableTokens: 340_319,
          estimatedConversationTokens: 142_031,
          contextWindow: 480_000,
          compactionThreshold: 0.75,
          compactionThresholdTokens: 360_000,
          compactionStrategy: "StripThenSummarize",
          durableMessageCount: 804,
          providerMessageCount: 612,
          totalCompactedMessages: 40,
          compactions: [
            {
              compactionKey: "session-1:1",
              sequence: 1,
              messagesCompacted: 40,
              originalTokens: 263_000,
              compactedTokens: 22_000,
              createdAt: "2026-08-24T12:00:00Z",
            },
          ],
          lastRequest: {
            requestId: "request-1",
            callId: "call-4",
            callSequence: 4,
            turnIndex: 3,
            attempt: 1,
            estimator: "serialized_json_bytes_div_4_v1",
            estimatedInputTokens: 182_631,
            contextWindow: 480_000,
            compactionThresholdTokens: 262_080,
            configuredMaxOutputTokens: 64_000,
            effectiveMaxOutputTokens: 64_000,
            compactionReason: "below_threshold",
            preCompactionInputTokens: null,
            components: {
              messages: 150_000,
              documents: 1_000,
              toolSchemas: 27_400,
              additionalParameters: 1_200,
              outputSchema: 3_031,
            },
          },
        }}
        runtimeHealth={null}
        selectedConversationTitle="weekend triage"
        selectedSessionId="session-1"
        onRenameConversationTitle={vi.fn()}
      />,
    );

    expect(screen.getByText("Context ≈183k / 480k")).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "182631");
    expect(screen.getByText("Last provider request")).toBeInTheDocument();
    expect(screen.getByText("Below threshold")).toBeInTheDocument();
    expect(screen.getByText("27,400")).toBeInTheDocument();
    expect(screen.getByText("142,031")).toBeInTheDocument();
    expect(screen.getByText("262,080 (55%)")).toBeInTheDocument();
    expect(screen.getByText("198,288 (58%)")).toBeInTheDocument();
    expect(screen.getByText("1 durable compaction")).toBeInTheDocument();
    expect(screen.getByText("263,000 → 22,000 tokens")).toBeInTheDocument();
  });
});

describe("assistant reasoning order", () => {
  it("renders completed reasoning before the assistant answer", () => {
    render(
      <MessageList
        timelineItems={[
          {
            kind: "assistantMessage",
            itemKey: "assistant-1",
            content: "Final answer",
            reasoning: "First I thought about it",
          },
        ]}
      />,
    );

    expectReasoningBeforeAnswer();
  });

  it("renders streaming reasoning before the live assistant answer", () => {
    render(
      <MessageList
        timelineItems={[
          {
            kind: "liveAssistant",
            itemKey: "live-1",
            content: "Answer in progress",
            reasoning: "Thinking in progress",
          },
        ]}
      />,
    );

    expectReasoningBeforeAnswer();
  });
});

describe("ChatTranscriptPanel states", () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, "scrollTo");
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("shows a loading skeleton (not the first-message empty state) while a selected conversation loads", () => {
    render(<ChatTranscriptPanel selectedSessionId="s1" session={null} />);
    expect(screen.getByTestId("transcript-loading")).toBeInTheDocument();
    expect(screen.queryByText("Send the first message")).not.toBeInTheDocument();
  });

  it("reserves the first-message empty state for no selected session", () => {
    render(<ChatTranscriptPanel selectedSessionId={null} session={null} />);
    expect(screen.getByText("Send the first message")).toBeInTheDocument();
    expect(screen.queryByTestId("transcript-loading")).not.toBeInTheDocument();
  });

  it("shows the acknowledged send immediately while the session snapshot catches up", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={null}
        optimisticPendingTurn={{
          sessionId: "s1",
          requestId: "req_2",
          content: "check the upgrade",
          selectedSkillIds: [],
          lifecycleState: "pending",
          createdAt: "2026-08-20T19:40:12Z",
        }}
      />,
    );

    expect(screen.getByText("check the upgrade")).toBeInTheDocument();
    expect(screen.getByTestId("request-progress")).toHaveTextContent("Queued");
    expect(screen.queryByTestId("transcript-loading")).not.toBeInTheDocument();
  });

  it("hands the optimistic turn to the matching durable request without duplicating it", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems: [pendingTurn] })}
        optimisticPendingTurn={{
          sessionId: "s1",
          requestId: "req_1",
          content: "do the thing",
          selectedSkillIds: [],
          lifecycleState: "pending",
          createdAt: null,
        }}
      />,
    );

    expect(screen.getAllByText("do the thing")).toHaveLength(1);
    expect(screen.getByTestId("request-progress")).toHaveTextContent("Working");
  });

  it("hands the optimistic turn to the matching durable user message", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          timelineItems: [
            {
              kind: "userMessage",
              itemKey: "user_2",
              requestId: "req_2",
              content: "check the upgrade",
            },
          ],
        })}
        optimisticPendingTurn={{
          sessionId: "s1",
          requestId: "req_2",
          content: "check the upgrade",
          selectedSkillIds: [],
          lifecycleState: "pending",
          createdAt: null,
        }}
      />,
    );

    expect(screen.getAllByText("check the upgrade")).toHaveLength(1);
    expect(screen.queryByTestId("request-progress")).not.toBeInTheDocument();
  });

  it("shows the thinking indicator while the turn runs and the assistant is silent", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems: [pendingTurn] })}
      />,
    );
    expect(screen.getByTestId("assistant-thinking")).toBeInTheDocument();
  });

  it("hides the thinking indicator once live assistant content streams", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          timelineItems: [
            pendingTurn,
            { kind: "liveAssistant", itemKey: "l1", content: "first tokens" },
          ],
        })}
      />,
    );
    expect(screen.queryByTestId("assistant-thinking")).not.toBeInTheDocument();
  });

  it("hides the thinking indicator on terminal turn states", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          turnState: "completed",
          timelineItems: [pendingTurn],
        })}
      />,
    );
    expect(screen.queryByTestId("assistant-thinking")).not.toBeInTheDocument();
  });

  it("follows the transcript with instant (not smooth) scrolling on a fresh send", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems: [pendingTurn] })}
      />,
    );

    expect(HTMLElement.prototype.scrollTo).toHaveBeenCalledWith({
      top: expect.any(Number),
      behavior: "instant",
    });
  });

  it("keeps following when a streaming assistant before the tail grows", () => {
    const trailingTool: RenderedTimelineItem = {
      kind: "toolGroup",
      itemKey: "tools-1",
      tools: [
        {
          itemKey: "tool-1",
          toolName: "bash",
          status: "running",
          statusKind: "running",
          presentation: {
            kind: "generic",
            summary: "still running",
            input: null,
            output: null,
          },
        },
      ],
    };
    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          timelineItems: [
            { kind: "liveAssistant", itemKey: "live-1", content: "a" },
            trailingTool,
          ],
        })}
      />,
    );
    vi.mocked(HTMLElement.prototype.scrollTo).mockClear();

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          timelineItems: [
            {
              kind: "liveAssistant",
              itemKey: "live-1",
              content: "a much longer streamed answer",
            },
            trailingTool,
          ],
        })}
      />,
    );

    expect(HTMLElement.prototype.scrollTo).toHaveBeenCalledWith({
      top: expect.any(Number),
      behavior: "instant",
    });
  });

  it("keeps following when tool presentation output changes", () => {
    const toolGroup = (output: string): RenderedTimelineItem => ({
      kind: "toolGroup",
      itemKey: "tools-1",
      tools: [
        {
          itemKey: "tool-1",
          toolName: "bash",
          status: "running",
          statusKind: "running",
          presentation: {
            kind: "generic",
            summary: "still running",
            input: null,
            output,
          },
        },
      ],
    });
    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems: [toolGroup("first")] })}
      />,
    );
    vi.mocked(HTMLElement.prototype.scrollTo).mockClear();

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          timelineItems: [toolGroup("first\nsecond\nthird")],
        })}
      />,
    );

    expect(HTMLElement.prototype.scrollTo).toHaveBeenCalledWith({
      top: expect.any(Number),
      behavior: "instant",
    });
  });

  it("loads an existing conversation at its tip", () => {
    const timelineItems: RenderedTimelineItem[] = Array.from(
      { length: 90 },
      (_, index) => ({
        kind: "userMessage",
        itemKey: `loaded-user-${index}`,
        content: `loaded-message-${index}`,
      }),
    );
    const { rerender } = render(
      <ChatTranscriptPanel selectedSessionId="s1" session={null} />,
    );
    const panel = screen.getByTestId("transcript-panel");
    Object.defineProperties(panel, {
      scrollHeight: { configurable: true, value: 1000 },
      scrollTop: { configurable: true, value: 0, writable: true },
      clientHeight: { configurable: true, value: 400 },
    });

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems })}
      />,
    );

    expect(panel.scrollHeight - panel.clientHeight - panel.scrollTop).toBe(0);
    expect(screen.queryByText("loaded-message-49")).not.toBeInTheDocument();
    expect(screen.getByText("loaded-message-50")).toBeInTheDocument();

    panel.scrollTop = 100;
    fireEvent.scroll(panel);
    expect(screen.queryByText("loaded-message-49")).not.toBeInTheDocument();
  });

  it("re-engages follow when an identical retry is first observed as a materialized user message", async () => {
    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          turnState: "failed",
          latestRequestId: "req_1",
          timelineItems: [
            {
              kind: "userMessage",
              itemKey: "user_1",
              content: "do the thing",
            },
          ],
        })}
      />,
    );

    const panel = screen.getByTestId("transcript-panel");
    Object.defineProperties(panel, {
      scrollHeight: { configurable: true, value: 1000 },
      scrollTop: { configurable: true, value: 100, writable: true },
      clientHeight: { configurable: true, value: 400 },
    });
    fireEvent.scroll(panel);

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({
          latestRequestId: "req_2",
          timelineItems: [
            {
              kind: "userMessage",
              itemKey: "user_1",
              content: "do the thing",
            },
            {
              kind: "userMessage",
              itemKey: "user_2",
              content: "do the thing",
            },
          ],
        })}
      />,
    );

    await waitFor(() =>
      expect(panel.scrollHeight - panel.clientHeight - panel.scrollTop).toBe(0),
    );
  });

  it("re-engages follow from the optimistic request before latestRequestId catches up", async () => {
    const staleSession = makeSession({
      latestRequestId: "req_old",
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: "user_old",
          requestId: "req_old",
          content: "earlier",
        },
      ],
    });
    const { rerender } = render(
      <ChatTranscriptPanel selectedSessionId="s1" session={staleSession} />,
    );

    const panel = screen.getByTestId("transcript-panel");
    Object.defineProperties(panel, {
      scrollHeight: { configurable: true, value: 1000 },
      scrollTop: { configurable: true, value: 100, writable: true },
      clientHeight: { configurable: true, value: 400 },
    });
    fireEvent.scroll(panel);

    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={staleSession}
        optimisticPendingTurn={{
          sessionId: "s1",
          requestId: "req_new",
          content: "follow up",
          selectedSkillIds: [],
          lifecycleState: "pending",
          createdAt: "2026-08-20T20:00:00Z",
        }}
      />,
    );

    await waitFor(() =>
      expect(panel.scrollHeight - panel.clientHeight - panel.scrollTop).toBe(0),
    );
  });

  it("renders a trailing page and prepends older messages without moving the reading position", async () => {
    const timelineItems: RenderedTimelineItem[] = Array.from(
      { length: 90 },
      (_, index) => ({
        kind: "userMessage",
        itemKey: `user-${index}`,
        content: `message-${index}`,
      }),
    );
    const { rerender } = render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems })}
      />,
    );

    const panel = screen.getByTestId("transcript-panel");
    Object.defineProperties(panel, {
      scrollHeight: {
        configurable: true,
        get: () => panel.querySelectorAll(".message-card").length * 20 + 200,
      },
      scrollTop: { configurable: true, value: 0, writable: true },
      clientHeight: { configurable: true, value: 400 },
    });

    expect(screen.queryByText("message-49")).not.toBeInTheDocument();
    expect(screen.getByText("message-50")).toBeInTheDocument();
    expect(screen.getByText("message-89")).toBeInTheDocument();
    const retainedMessage = screen.getByText("message-50");

    fireEvent.click(screen.getByTestId("transcript-load-older"));

    await waitFor(() => expect(screen.getByText("message-10")).toBeInTheDocument());
    expect(screen.queryByText("message-9")).not.toBeInTheDocument();
    expect(screen.getByText("message-50")).toBe(retainedMessage);
    expect(retainedMessage.isConnected).toBe(true);
    expect(panel.scrollTop).toBe(800);

    const streamingTimelineItems: RenderedTimelineItem[] = [
      ...timelineItems,
      {
        kind: "assistantMessage",
        itemKey: "assistant-90",
        content: "message-90",
      },
    ];
    rerender(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={makeSession({ timelineItems: streamingTimelineItems })}
      />,
    );

    expect(screen.getByText("message-10")).toBeInTheDocument();
    expect(screen.getByText("message-90")).toBeInTheDocument();

    panel.scrollTop = panel.scrollHeight - panel.clientHeight;
    fireEvent.scroll(panel);

    await waitFor(() =>
      expect(screen.queryByText("message-10")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("message-11")).toBeInTheDocument();
  });

  it("requests exactly one remote page and retains the mounted tip rows", async () => {
    const allItems: RenderedTimelineItem[] = Array.from({ length: 80 }, (_, index) => ({
      kind: "userMessage",
      itemKey: `remote-${index}`,
      content: `remote-message-${index}`,
    }));
    const onLoad = vi.fn();

    function RemotePageFixture() {
      const [session, setSession] = useState(
        makeSession({
          timelineItems: allItems.slice(40),
          timelinePage: {
            totalItems: 80,
            pageItems: 40,
            hasOlder: true,
            hasNewer: false,
            oldestItemKey: "remote-40",
            newestItemKey: "remote-79",
          },
        }),
      );
      return (
        <ChatTranscriptPanel
          selectedSessionId="s1"
          session={session}
          onLoadOlder={async () => {
            onLoad();
            setSession((current) => ({
              ...current,
              timelineItems: allItems,
              timelinePage: {
                totalItems: 80,
                pageItems: 80,
                hasOlder: false,
                hasNewer: false,
                oldestItemKey: "remote-0",
                newestItemKey: "remote-79",
              },
            }));
            return true;
          }}
        />
      );
    }

    render(<RemotePageFixture />);
    const retainedMessage = screen.getByText("remote-message-40");
    fireEvent.click(screen.getByTestId("transcript-load-older"));

    await waitFor(() =>
      expect(screen.getByText("remote-message-0")).toBeInTheDocument(),
    );
    expect(onLoad).toHaveBeenCalledTimes(1);
    expect(screen.getByText("remote-message-40")).toBe(retainedMessage);
    expect(screen.queryByTestId("transcript-load-older")).not.toBeInTheDocument();
  });
});
