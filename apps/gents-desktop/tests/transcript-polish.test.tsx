import { act, fireEvent, screen, within } from "@testing-library/react";
import type { ReactElement } from "react";
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

import { BackgroundToolButton } from "../src/ui/screens/BackgroundToolButton";
import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { ToolBody } from "../src/ui/screens/tool-views";
import {
  diffShape,
  diffText,
  lineCount,
  toolSummary,
} from "../src/ui/screens/tool-summary";
import {
  activityStatus,
  interruptTarget,
  isStopping,
} from "../src/ui/screens/activity-status";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { renderIn, testApp } from "./app-fixture";
import { liveAssistant } from "./timeline-fixture";

const render = (ui: ReactElement) => renderIn(testApp(), ui);

function tool(
  presentation: RenderedToolCallView["presentation"],
  statusKind = "success",
): RenderedToolCallView {
  return {
    itemKey: "tool-1",
    toolName: "tool",
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
    nodeDid: "did:test:node",
    agentId: "agent-default",
    title: "Session",
    previewText: null,
    status: "processing",
    goal: null,
    turnState: "processing",
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
      durableMessageCount: 0,
      providerMessageCount: 0,
      totalCompactedMessages: 0,
      sessionCompactionCount: null,
      requestReductionHistoryLoaded: false,
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
    inFlight={inFlight}
    stopping={stopping}
    scroller={null}
    session={snapshot}
    workers={NO_WORKERS}
    parentWork={NO_PARENT}
    workerActions={{} as never}
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
  const editWith = (
    diff: { kind: "added" | "removed" | "context"; text: string }[],
    created = false,
  ) => tool({ ...edit.presentation, created, diff } as never);

  it("shows a replacement as what was replaced and what replaced it", () => {
    render(
      <ToolBody
        tool={editWith([
          { kind: "removed", text: "old line" },
          { kind: "added", text: "new line" },
        ])}
      />,
    );
    expect(screen.getByText("replaced")).toBeInTheDocument();
    expect(screen.getByText("with")).toBeInTheDocument();
    expect(screen.getByText("old line").closest("[data-diff]")).toHaveAttribute(
      "data-diff",
      "removed",
    );
  });

  it("shows a new file as code, and an addition to a file in the added colour", () => {
    const lines = [{ kind: "added" as const, text: "fn main() {}" }];
    const { unmount } = render(<ToolBody tool={editWith(lines, true)} />);
    expect(screen.getByText("new file")).toBeInTheDocument();
    expect(screen.getByText("fn main() {}").closest("pre")).not.toHaveClass(
      "bg-diff-added",
    );
    unmount();
    render(<ToolBody tool={editWith(lines)} />);
    expect(screen.getByText("added")).toBeInTheDocument();
    expect(screen.getByText("fn main() {}").closest("pre")).toHaveClass(
      "bg-diff-added",
    );
  });

  it("keeps a unified diff's gutter, in the diff colours", () => {
    render(<ToolBody tool={edit} />);
    const mark = (text: string) =>
      screen.getByText(text).closest("[data-diff]")?.querySelector("[aria-hidden]")
        ?.textContent;
    expect([mark("fn b() {}"), mark("fn c() {}"), mark("fn c2() {}")]).toEqual([
      " ",
      "\u2212",
      "+",
    ]);
    expect(screen.getByText("fn c2() {}").closest("[data-diff]")).toHaveClass(
      "bg-diff-added",
    );
    expect(screen.getByText("fn c() {}").closest("[data-diff]")).toHaveClass(
      "bg-diff-removed",
    );
  });

  it("takes the shape of the edit", () => {
    const of = (...kinds: ("added" | "removed" | "context")[]) =>
      diffShape(kinds.map((kind) => ({ kind })));
    expect(of("added", "added")).toBe("addition");
    expect(of("removed")).toBe("removal");
    expect(of("removed", "removed", "added")).toBe("replacement");
    expect(of("removed", "added", "removed")).toBe("unified");
    expect(of("added", "removed")).toBe("unified");
    expect(of("context", "added")).toBe("unified");
    expect(of("context")).toBe("unified");
  });

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
    const observers = new Set<() => void>();
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(private readonly callback: () => void) {}
        observe() {
          observers.add(this.callback);
        }
        disconnect() {
          observers.delete(this.callback);
        }
      },
    );
    const grow = () => act(() => observers.forEach((notify) => notify()));
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
    grow();
    expect(viewport.scrollTop).toBe(500 - 160);

    act(() => {
      viewport.dispatchEvent(new Event("wheel"));
      viewport.scrollTop = 0;
      viewport.dispatchEvent(new Event("scroll"));
    });
    scrollHeight = 900;
    rerender(<ToolBody tool={running("a\nb\nc")} />);
    grow();
    expect(viewport.scrollTop).toBe(0);
    vi.unstubAllGlobals();
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
        [liveAssistant({ kind: "liveAssistant", itemKey: "live", content: "Here" })],
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

describe("a folded group with a failure in it", () => {
  const command = (key: string, failed: boolean): RenderedToolCallView => ({
    ...tool({
      kind: "command",
      command: failed ? "cargo test" : "ls",
      exitCode: failed ? 101 : 0,
      timedOut: false,
      failed,
      durationMs: 10,
      cwd: "~",
      executionMode: "sandboxed",
      networkMode: "disabled",
      stdout: "",
      stderr: "",
      fallbackOutput: null,
    }),
    itemKey: key,
  });
  const group = (tools: RenderedToolCallView[]): RenderedTimelineItem => ({
    kind: "toolGroup",
    itemKey: "group",
    messageSequence: 1,
    tools,
  });
  const header = () =>
    screen.getByRole("button", { name: /ran 2 commands/i, expanded: undefined });

  it("names the failure on the header and starts open", () => {
    render(
      panel(
        session({ timelineItems: [group([command("a", false), command("b", true)])] }),
        false,
      ),
    );
    const button = header();
    expect(button).toHaveTextContent("Ran 2 commands, 1 failed");
    expect(button).toHaveAttribute("aria-expanded", "true");
    expect(button).toHaveAttribute("data-failed", "true");
  });

  it("stays folded when every call worked", () => {
    render(
      panel(
        session({ timelineItems: [group([command("a", false), command("b", false)])] }),
        false,
      ),
    );
    const button = header();
    expect(button).toHaveTextContent("Ran 2 commands");
    expect(button).not.toHaveTextContent("failed");
    expect(button).toHaveAttribute("aria-expanded", "false");
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

  it.each([undefined, false, true])(
    "uses only positive runtime evidence for stopped-before-running (%s)",
    (certificate) => {
      render(
        panel(
          session({
            turnState: "interrupted",
            latestRequestOutcome: {
              failureReason: null,
              cancelCause: interruptEvidence,
              stoppedBeforeRunning: certificate,
            },
          }),
          false,
        ),
      );
      const notice = screen.getByTestId("stopped-notice");
      expect(
        within(notice).getByText(
          certificate === true ? "Stopped before running" : "Stopped",
        ),
      ).toBeVisible();
    },
  );

  it("stops the running turn while a follow-up message is still syncing", () => {
    expect(
      interruptTarget({
        trackedRequestId: "new-queued-message",
        latestRequestId: "running-foreground-bash",
        turnState: "running",
      }),
    ).toBe("running-foreground-bash");
  });

  it("does not interrupt a completed predecessor instead of a new request", () => {
    expect(
      interruptTarget({
        trackedRequestId: "new-request",
        latestRequestId: "completed-predecessor",
        turnState: "completed",
      }),
    ).toBe("new-request");
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

describe("foreground command control", () => {
  const command = {
    ...tool({ kind: "generic", summary: null, input: null, output: null }, "running"),
    toolName: "bash_unrestricted",
    toolCallId: "command-1",
    requestId: "request-1",
    awaitMode: "foreground",
    backgroundAvailable: true,
  };

  it("backgrounds the exact running command without stopping its request", async () => {
    const backgroundTool = vi.fn().mockResolvedValue(true);
    const app = testApp({ api: { backgroundTool }, session: session({}) });
    renderIn(app, <BackgroundToolButton tool={command} />);
    await act(async () => {
      const button = screen.getByRole("button", { name: "Run in background" });
      fireEvent.click(button);
      fireEvent.click(button);
    });
    expect(backgroundTool).toHaveBeenCalledWith({
      nodeDid: "did:test:node",
      sessionId: "session-1",
      requestId: "request-1",
      toolCallId: "command-1",
    });
    expect(screen.getByRole("button", { name: "Backgrounding…" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Backgrounding…" }));
    expect(backgroundTool).toHaveBeenCalledTimes(1);
  });

  it("can retry a failed handoff and settles when the command already finished", async () => {
    const backgroundTool = vi
      .fn()
      .mockRejectedValueOnce(new Error("runtime unavailable"))
      .mockResolvedValueOnce(false);
    const app = testApp({ api: { backgroundTool }, session: session({}) });
    renderIn(app, <BackgroundToolButton tool={command} />);
    for (let attempt = 0; attempt < 2; attempt++) {
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Run in background" }));
      });
      expect(screen.getByRole("button", { name: "Run in background" })).toBeEnabled();
    }
    expect(backgroundTool).toHaveBeenCalledTimes(2);
  });

  it("does not offer backgrounding for a completed command", () => {
    render(
      <BackgroundToolButton
        tool={{
          ...edit,
          toolName: "bash_unrestricted",
          toolCallId: "command-1",
          requestId: "request-1",
          awaitMode: "foreground",
        }}
      />,
    );
    expect(screen.queryByRole("button", { name: "Run in background" })).toBeNull();
  });
});
