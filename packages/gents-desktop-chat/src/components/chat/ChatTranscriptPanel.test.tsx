import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

import { ChatTranscriptPanel } from "./ChatTranscriptPanel.js";
import type { OptimisticPendingTurn } from "../../chat-shell.js";

const session: DesktopSessionSnapshot = {
  sessionId: "session",
  nodeDid: "did:runtime",
  agentId: "engineer",
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
  timelineItems: [],
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
};
const pending: OptimisticPendingTurn = {
  sessionId: "session",
  requestId: "request",
  content: "Sent without waiting for a receipt",
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt: null,
};

test("compacts invisible action separators before paging without losing the final turn attachment", () => {
  const requestTurn = {
    requestDocId: "physical",
    requestId: "request",
    settled: false,
  };
  const timelineItems: RenderedTimelineItem[] = Array.from(
    { length: 1000 },
    (_, index) => [
      {
        kind: "toolGroup" as const,
        itemKey: `group-${index}`,
        tools: [
          {
            itemKey: `action-${index}`,
            toolName: "fixture",
            statusKind: index >= 998 ? "running" : "success",
            requestTurn,
            reconstruction: { state: "ready" as const },
            presentation: {
              kind: "generic" as const,
              summary: null,
              input: null,
              output: null,
            },
          },
        ],
      },
      {
        kind: "assistantMessage" as const,
        itemKey: `empty-${index}`,
        content: "",
        requestTurn,
        reconstruction: { state: "ready" as const },
      },
    ],
  ).flat();
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel
      selectedSessionId="session"
      session={{ ...session, timelineItems }}
      compactTools
      activity={null}
      renderAfterItem={(item) =>
        item.itemKey === "empty-999" ? (
          <section>Completed change attachment</section>
        ) : null
      }
    />,
  );
  expect(html.match(/class="tool-activity-row/g)).toHaveLength(3);
  expect(html).toContain("Ran 998 actions");
  expect(html).toContain("Completed change attachment");
  expect(html).not.toContain("Load older messages");
});

test("renders host activity after the pending input without a second thinking card", () => {
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel
      selectedSessionId="session"
      session={session}
      optimisticPendingTurn={pending}
      activity={<div role="status">Thinking</div>}
    />,
  );
  expect(html.indexOf("Thinking")).toBeGreaterThan(
    html.indexOf(pending.content),
  );
  expect(html.match(/role="status">Thinking/g)).toHaveLength(1);
  expect(html).not.toContain('data-testid="assistant-thinking"');
});

test("preserves the default activity when the host does not supply a slot", () => {
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel selectedSessionId="session" session={session} />,
  );
  expect(html).toContain('data-testid="assistant-thinking"');
});

test("an explicitly empty activity slot suppresses the default card", () => {
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel
      selectedSessionId="session"
      session={session}
      activity={null}
    />,
  );
  expect(html).not.toContain('data-testid="assistant-thinking"');
});

test("keeps attachment-only optimistic input visible before its receipt", () => {
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel
      selectedSessionId="session"
      session={session}
      optimisticPendingTurn={{
        ...pending,
        content: "",
        capturedContext: [
          {
            kind: "file",
            source: "editor",
            uri: "file:///project/scratch.rs",
            capturedAt: "2026-10-20T00:00:00Z",
            terminalName: null,
            workspaceUri: "file:///project",
            version: 3,
            unsaved: true,
            range: null,
            content: "captured buffer",
          },
        ],
      }}
      activity={<div role="status">Sending message</div>}
    />,
  );
  expect(html).toContain("scratch.rs");
  expect(html).toContain("Sending message");
});

test("keeps turn attachments inside their measured transcript row", () => {
  const html = renderToStaticMarkup(
    <ChatTranscriptPanel
      selectedSessionId="session"
      session={{
        ...session,
        timelineItems: [
          {
            kind: "assistantMessage",
            itemKey: "answer",
            content: "Finished answer",
            sequence: null,
            reasoning: null,
            timestamp: null,
            reconstruction: { state: "ready" },
          },
        ],
      }}
      activity={null}
      renderAfterItem={(item) => (
        <section data-after={item.itemKey}>Recorded changes</section>
      )}
    />,
  );
  const after = html.indexOf('data-after="answer"');
  expect(after).toBeGreaterThan(html.indexOf("Finished answer"));
  expect(after).toBeLessThan(html.indexOf("</li>"));
  expect(html.match(/Recorded changes/g)).toHaveLength(1);
});

test("keeps confirmed opening attribution through pending-to-durable handoff", () => {
  const render = (
    optimisticPendingTurn: OptimisticPendingTurn | undefined,
    timelineItems: RenderedTimelineItem[] = [],
  ) =>
    renderToStaticMarkup(
      <ChatTranscriptPanel
        selectedSessionId="session"
        session={{ ...session, timelineItems }}
        optimisticPendingTurn={optimisticPendingTurn}
        activity={null}
      />,
    );
  expect(render(pending)).not.toContain("Sent as goal");
  const confirmed = { ...pending, startedGoalId: "goal-1" };
  expect(render(confirmed)).toContain("Sent as goal");
  const durable: RenderedTimelineItem = {
    kind: "userMessage",
    itemKey: "authored:physical:prompt",
    requestId: pending.requestId,
    inputRequestId: pending.requestId,
    ownsTurn: true,
    content: pending.content,
    startedGoalId: "goal-1",
    timestamp: null,
    reconstruction: { state: "ready" },
  };
  expect(render(confirmed, [durable]).match(/Sent as goal/g)).toHaveLength(1);
  expect(render(undefined, [durable]).match(/Sent as goal/g)).toHaveLength(1);
});
