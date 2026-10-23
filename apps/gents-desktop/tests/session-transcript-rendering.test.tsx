import { act, fireEvent, screen } from "@testing-library/react";
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

import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { renderIn, testApp } from "./app-fixture";
import { assistantMessage } from "./timeline-fixture";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";

const UNRELATED = {
  workers: NO_WORKERS,
  parentWork: NO_PARENT,
  workerActions: { interrupt: () => {} },
};

function session(content: string): DesktopSessionSnapshot {
  return {
    sessionId: "session-long",
    nodeDid: "did:test:node",
    agentId: "agent-default",
    title: "Long session",
    previewText: content,
    status: "completed",
    goal: null,
    turnState: "completed",
    latestRequestId: "request-1",
    retryEligibility: { eligible: false, denialReason: null },
    latestRequestOutcome: null,
    pendingTurn: null,
    queuedTurns: [],
    foldedInputs: [],
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 1,
      compactionThreshold: 0,
      compactionThresholdTokens: 0,
      compactionStrategy: "StripThenSummarize",
      durableMessageCount: 1,
      providerMessageCount: 1,
      totalCompactedMessages: 0,
      sessionCompactionCount: null,
      requestReductionHistoryLoaded: false,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: [
      assistantMessage({
        kind: "assistantMessage",
        itemKey: "assistant-1",
        sequence: 1,
        content,
        reasoning: null,
        timestamp: null,
        reconstruction: { state: "ready" },
      }),
    ],
  };
}

describe("SessionScreen transcript render boundary", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("renders failures and retries through the app's action, then shows interruption", async () => {
    const failed = {
      ...session("partial response"),
      turnState: "failed",
      retryEligibility: { eligible: true, denialReason: null },
    };
    const app = testApp();
    const retry = vi.spyOn(app.actions, "retryMessage").mockResolvedValue(undefined);
    const props = { ...UNRELATED, inFlight: false, scroller: null, content: null };
    const view = renderIn(app, <TranscriptPanel {...props} session={failed} />);
    expect(screen.getByText("The assistant could not finish this turn.")).toBeVisible();
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "Retry" })),
    );
    expect(retry).toHaveBeenCalledWith("request-1");
    view.rerender(
      <TranscriptPanel {...props} session={{ ...failed, turnState: "interrupted" }} />,
    );
    expect(screen.getByText("This response was stopped.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
  });

  it("keeps unchanged rows out of unrelated session projection renders", () => {
    markdownRender.mockClear();
    const original = session("stable markdown");
    const props = { ...UNRELATED, inFlight: false, scroller: null, content: null };
    const view = renderIn(testApp(), <TranscriptPanel {...props} session={original} />);

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
});
