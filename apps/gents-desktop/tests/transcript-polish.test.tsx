import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { TranscriptPanel } from "../src/ui/screens/SessionScreen";
import { ToolBody } from "../src/ui/screens/tool-views";
import { diffText, lineCount, toolSummary } from "../src/ui/screens/tool-summary";
import { activityStatus, isStopping } from "../src/ui/screens/activity-status";

function tool(
  presentation: RenderedToolCallView["presentation"],
  statusKind = "success",
): RenderedToolCallView {
  return {
    itemKey: "tool-1",
    toolName: "tool",
    status: "completed",
    statusKind,
    presentation,
    reconstruction: { state: "ready" },
  };
}

const edit = tool({
  kind: "fileEdit",
  operation: "edit_file",
  path: "src/lib.rs",
  created: false,
  replacementsApplied: 1,
  diff: [
    { kind: "context", text: "fn b() {}" },
    { kind: "removed", text: "fn c() {}" },
    { kind: "added", text: "fn c2() {}" },
  ],
  fallbackOutput: null,
});

function session(overrides: Partial<DesktopSessionSnapshot>): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:test:agent",
    behaviorId: "behavior-default",
    title: "Session",
    previewText: null,
    status: "processing",
    turnState: "processing",
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
      durableMessageCount: 0,
      providerMessageCount: 0,
      totalCompactedMessages: 0,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: [],
    ...overrides,
  };
}

const panel = (
  snapshot: DesktopSessionSnapshot,
  inFlight: boolean,
  stopping = false,
) => (
  <TranscriptPanel
    actionsRef={{
      current: {
        loadOlderSessionTimeline: vi.fn(async () => false),
        retryMessage: vi.fn(async () => null),
      },
    }}
    holdsCount={0}
    inFlight={inFlight}
    stopping={stopping}
    ownerRef={{ current: null }}
    session={snapshot}
    workers={{ byChildRequest: () => null } as never}
    parentWork={{ parent: null, sentBy: () => null } as never}
    workerActions={{} as never}
    deployment={null}
  />
);

const interruptEvidence = {
  cause: "userCancelled",
  source: "requestInterrupt",
  confidence: "direct",
  at: "2026-09-23T03:03:34Z",
  evidence: ["AgentRequest.interrupt_requested_at = 2026-09-23T03:03:34Z"],
};

describe("file edit diff", () => {
  it("marks added, removed and unchanged lines by kind", () => {
    const { container } = render(<ToolBody tool={edit} />);
    const kind = (text: string) =>
      screen.getByText(text).closest("[data-diff]")?.getAttribute("data-diff");
    expect(kind("fn c2() {}")).toBe("added");
    expect(kind("fn c() {}")).toBe("removed");
    expect(kind("fn b() {}")).toBe("context");
    expect(container.querySelectorAll("[data-diff=removed]")).toHaveLength(1);
    expect(
      diffText(edit.presentation.kind === "fileEdit" ? edit.presentation.diff : []),
    ).toBe(" fn b() {}\n-fn c() {}\n+fn c2() {}");
  });
});

function command(
  fields: Partial<Extract<RenderedToolCallView["presentation"], { kind: "command" }>>,
): RenderedToolCallView["presentation"] {
  return {
    kind: "command",
    command: "cargo test",
    exitCode: 0,
    timedOut: false,
    failed: false,
    durationMs: null,
    cwd: null,
    executionMode: null,
    networkMode: null,
    stdout: "",
    stderr: "",
    fallbackOutput: null,
    ...fields,
  };
}

describe("long command output", () => {
  it("says how many lines a multi-line output holds", () => {
    const stdout =
      Array.from({ length: 1200 }, (_, i) => `line ${i}`).join("\n") + "\n";
    render(<ToolBody tool={tool(command({ stdout, stderr: "one warning" }))} />);
    expect(screen.getByText(/1,200 lines/)).toBeInTheDocument();
    expect(screen.getAllByText(/ lines$/)).toHaveLength(1);
  });

  it("counts lines without an extra one for the final newline", () => {
    expect(lineCount("")).toBe(0);
    expect(lineCount("a")).toBe(1);
    expect(lineCount("a\n")).toBe(1);
    expect(lineCount("a\r\nb\r\n")).toBe(2);
  });

  it("keeps a running command's live output at its newest line until the reader scrolls up", () => {
    const running = (tail: string) => ({
      ...tool(command({}), "running"),
      partialOutputTail: tail,
    });
    const { rerender } = render(<ToolBody tool={running("a")} />);
    const viewport = screen
      .getByTestId("tool-live-output-tool-1")
      .querySelector<HTMLElement>("[data-slot=scroll-area-viewport]")!;
    let scrollHeight = 500;
    Object.defineProperties(viewport, {
      clientHeight: { configurable: true, get: () => 160 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
    });

    rerender(<ToolBody tool={running("a\nb")} />);
    expect(viewport.scrollTop).toBe(500);

    act(() => {
      viewport.scrollTop = 0;
      viewport.dispatchEvent(new Event("scroll"));
    });
    scrollHeight = 900;
    rerender(<ToolBody tool={running("a\nb\nc")} />);
    expect(viewport.scrollTop).toBe(0);
  });
});

describe("command content", () => {
  it("shows a command and its output as they ran", () => {
    const run = tool(
      command({
        command: "curl -H 'Authorization: Bearer x' example.com",
        stdout: "API_KEY=abc",
      }),
    );
    expect(toolSummary(run).primary).toBe(
      "curl -H 'Authorization: Bearer x' example.com",
    );
    render(<ToolBody tool={run} />);
    expect(screen.getByText("API_KEY=abc")).toBeInTheDocument();
  });
});

describe("activity status", () => {
  const running = tool(edit.presentation, "running");
  const group = (tools: RenderedToolCallView[]): RenderedTimelineItem => ({
    kind: "toolGroup",
    itemKey: "group",
    messageSequence: 1,
    tools,
  });

  it("names what the run is doing between actions", () => {
    expect(activityStatus([], false)).toBe("Thinking");
    expect(activityStatus([group([running])], false)).toBeNull();
    expect(activityStatus([group([edit])], false)).toBe("Reviewing results");
    expect(
      activityStatus(
        [{ kind: "liveAssistant", itemKey: "live", content: "Here" }],
        false,
      ),
    ).toBe("Writing");
    expect(activityStatus([group([running])], true)).toBe("Stopping…");
  });

  it("shows the status as a line of the run, not a pill", () => {
    render(panel(session({ timelineItems: [group([edit])] }), true));
    const status = screen.getByTestId("activity-status");
    expect(status).toHaveAttribute("role", "status");
    expect(status).toHaveTextContent("Reviewing results");
    expect(status.className).not.toContain("bg-lime");
  });
});

describe("stop", () => {
  it("shows Stopping while the request is still running", () => {
    render(panel(session({}), true, true));
    expect(screen.getByTestId("activity-status")).toHaveTextContent("Stopping…");
  });

  it("reports a terminal stop in plain language with lifecycle details behind a disclosure", () => {
    render(
      panel(
        session({
          turnState: "interrupted",
          latestRequestOutcome: { failureReason: null, cancelCause: interruptEvidence },
        }),
        false,
      ),
    );
    const notice = screen.getByTestId("stopped-notice");
    expect(within(notice).getByText("You stopped this response.")).toBeVisible();
    expect(notice).not.toHaveTextContent("AgentResponse");
    expect(notice).not.toHaveTextContent("responseInterruptedAt");
    fireEvent.click(within(notice).getByRole("button", { name: "Details" }));
    expect(
      within(notice).getByText(
        "AgentRequest.interrupt_requested_at = 2026-09-23T03:03:34Z",
      ),
    ).toBeInTheDocument();
    expect(
      within(notice).getByText("a stop request on this request"),
    ).toBeInTheDocument();
  });

  it("binds Stopping to the request being stopped", () => {
    const base = {
      inFlight: true,
      requestId: "request-b",
      latestRequestId: "request-b",
      interruptObserved: false,
      requestedStop: null,
    };
    expect(isStopping(base)).toBe(false);
    expect(isStopping({ ...base, requestedStop: "request-b" })).toBe(true);
    expect(isStopping({ ...base, interruptObserved: true })).toBe(true);
    expect(
      isStopping({ ...base, latestRequestId: "request-a", interruptObserved: true }),
    ).toBe(false);
    expect(isStopping({ ...base, requestedStop: "request-a" })).toBe(false);
    expect(isStopping({ ...base, inFlight: false, requestedStop: "request-b" })).toBe(
      false,
    );
  });

  it("does not claim the person stopped a response it has no cause for", () => {
    render(panel(session({ turnState: "interrupted" }), false));
    const notice = screen.getByTestId("stopped-notice");
    expect(notice).toHaveTextContent("This response was stopped.");
    expect(notice).not.toHaveTextContent("You stopped");
  });
});
