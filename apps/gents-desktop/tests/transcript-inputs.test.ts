import type {
  RenderedToolCallView,
  RequestOriginView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import type { FleetState } from "../src/hooks/fleetStore";
import {
  backgroundName,
  backgroundRunLabel,
  backgroundText,
  originLabel,
  senderSession,
} from "@/screens/TranscriptInputs";
import type { BackgroundResult } from "@/screens/transcript-groups";
import type { Reached } from "@/screens/workers";

describe("goal continuation labels", () => {
  it("distinguishes a recorded wrap-up from an ordinary continuation", () => {
    expect(
      originLabel(
        {
          kind: "goalContinuation",
          retry: null,
          goalId: "g",
          sequence: 3,
          wrapup: true,
        },
        null,
        null,
      ),
    ).toBe("Goal wrap-up");
    expect(
      originLabel(
        {
          kind: "goalContinuation",
          retry: null,
          goalId: "g",
          sequence: 3,
          wrapup: false,
        },
        null,
        null,
      ),
    ).toBe("Goal continuation · 3");
    expect(
      originLabel(
        { kind: "goalContinuation", retry: null, goalId: "g", sequence: null },
        null,
        null,
      ),
    ).toBe("Goal continuation");
  });
});

describe("naming a background result", () => {
  const completion = (
    status: string,
    tool = "bash_unrestricted",
  ): RequestOriginView => ({
    kind: "backgroundCompletion",
    notification: true,
    toolCallId: "call-1",
    toolCallDocId: "physical-call-1",
    toolName: tool,
    statusKind: status,
    reason: null,
    resultRange: null,
  });
  const step = (presentation: object) =>
    ({
      itemKey: "t",
      toolName: "bash_unrestricted",
      statusKind: "success",
      awaitMode: "background",
      presentation,
    }) as unknown as RenderedToolCallView;
  const command = (text: string) => step({ kind: "command", command: text });

  it("names the command a drawn step ran, and how it ended", () => {
    expect(
      backgroundText(
        backgroundName(completion("completed"), command("cd crates && cargo test")),
      ),
    ).toBe("cargo test finished");
    expect(
      backgroundText(backgroundName(completion("timed_out"), command("make"))),
    ).toBe("make timed out");
    expect(
      backgroundText(
        backgroundName(
          completion("completed"),
          step({ kind: "process", action: "spawn", target: "npm run dev" }),
        ),
      ),
    ).toBe("npm run dev finished");
  });

  it("names a worker the way its step does, by the session it reached", () => {
    const worker = step({
      kind: "agent",
      action: "start",
      name: "engineer",
      sessionId: "session-from-call",
    });
    const reached = {
      request: { sessionId: "session-worker" },
      summary: { title: "slow worker (engineer)", agentDid: "engineer" },
      worker: null,
    } as unknown as Reached;
    expect(backgroundName(completion("completed"), worker, reached)).toEqual({
      subject: "slow worker (engineer)",
      outcome: "finished",
      worker: { sessionId: "session-worker", agentDid: "engineer" },
    });
    expect(backgroundName(completion("failed"), worker)).toEqual({
      subject: "engineer",
      outcome: "failed",
      worker: { sessionId: "session-from-call", agentDid: null },
    });
  });

  it("names an undrawn call by its tool", () => {
    expect(backgroundText(backgroundName(completion("failed"), null))).toBe(
      "bash_unrestricted failed",
    );
    expect(backgroundText(backgroundName(completion("cancelled"), null))).toBe(
      "bash_unrestricted cancelled",
    );
  });

  it("names a wake from its native origin, independently of prompt wording", () => {
    const wake: RequestOriginView = {
      kind: "backgroundCompletion",
      notification: false,
      toolCallId: null,
      reason: null,
      resultRange: null,
    };
    expect(backgroundName(wake, null)).toBeNull();
    expect(backgroundText(null, wake)).toBe("Asked to review background results");
    expect(backgroundText(null, null)).toBe("Background result");
  });

  it("counts the completions a run folds", () => {
    const result = (origin: RequestOriginView | null): BackgroundResult =>
      ({ item: { origin }, call: null, prompts: [] }) as unknown as BackgroundResult;
    const other = result(null);
    expect(backgroundRunLabel([result(completion("completed")), other])).toBe(
      "bash_unrestricted finished",
    );
    expect(
      backgroundRunLabel([
        result(completion("completed")),
        result(completion("failed")),
        result(completion("completed")),
        other,
      ]),
    ).toBe("3 background results");
    expect(backgroundRunLabel([other, other])).toBe("2 background results");
  });
});

describe("finding who sent a message", () => {
  const SENDER = "did:key:z6MkSenderNodeWithALongIdentifier";
  const OTHER = "did:key:z6MkSomeOtherNodeEntirely0000";
  const listed = (nodeDid: string, sessionId: string) =>
    ({ nodeDid, sessionId, title: `${sessionId} title` }) as SessionSummary;
  const reviewer = listed(SENDER, "session-reviewer");
  const elsewhere = listed(OTHER, "session-elsewhere");
  const fleet = {
    byNodeSessionId: {
      [SENDER]: new Map([[reviewer.sessionId, reviewer]]),
      [OTHER]: new Map([[elsewhere.sessionId, elsewhere]]),
    },
    bySessionId: { [reviewer.sessionId]: reviewer, [elsewhere.sessionId]: elsewhere },
  } as unknown as FleetState;
  const origin = (
    over: Partial<Extract<RequestOriginView, { kind: "sessionMessage" }>> = {},
  ): Extract<RequestOriginView, { kind: "sessionMessage" }> => ({
    kind: "sessionMessage",
    senderNodeDid: SENDER,
    senderSessionId: null,
    senderRequestId: "request-1",
    ...over,
  });
  it("uses the native sender scope without reading a prompt header", () => {
    expect(senderSession(fleet, origin({ senderSessionId: "session-reviewer" }))).toBe(
      reviewer,
    );
    expect(
      senderSession(
        fleet,
        origin({ senderNodeDid: OTHER, senderSessionId: "session-reviewer" }),
      ),
    ).toBeNull();
    expect(
      senderSession(
        fleet,
        origin({ senderNodeDid: null, senderSessionId: "session-elsewhere" }),
      ),
    ).toBeNull();
    expect(senderSession(fleet, origin())).toBeNull();
  });
});
