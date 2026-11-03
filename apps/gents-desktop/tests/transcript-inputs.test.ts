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
  withoutRoutingHeader,
} from "@/screens/TranscriptInputs";
import type { BackgroundResult } from "@/screens/transcript-groups";
import type { Reached } from "@/screens/workers";

describe("goal continuation labels", () => {
  it("distinguishes a recorded wrap-up from an ordinary continuation", () => {
    expect(
      originLabel(
        { kind: "goalContinuation", goalId: "g", sequence: 3, wrapup: true },
        null,
        null,
      ),
    ).toBe("Goal wrap-up");
    expect(
      originLabel(
        { kind: "goalContinuation", goalId: "g", sequence: 3, wrapup: false },
        null,
        null,
      ),
    ).toBe("Goal continuation · 3");
    expect(
      originLabel(
        { kind: "goalContinuation", goalId: "g", sequence: null },
        null,
        null,
      ),
    ).toBe("Goal continuation");
  });
});

describe("a message's routing line", () => {
  const header =
    "[message agent:default session:5c1d node:did:key:z6Mk · at:2026-11-01T13:53:39Z · cause:9a7e]";

  it("is left out of the text a person reads", () => {
    expect(withoutRoutingHeader(`${header}\n\nthe build is green\n\nnext step`)).toBe(
      "the build is green\n\nnext step",
    );
    expect(withoutRoutingHeader(`${header}\r\n\r\nhello`)).toBe("hello");
    expect(withoutRoutingHeader(header)).toBe("");
  });

  it("is only a first line shaped like one", () => {
    for (const text of [
      "the build is green",
      "[message agent:default but no closing bracket\n\nbody",
      "[note] something else\n\nbody",
      `intro\n${header}\n\nbody`,
      ` ${header}\n\nbody`,
    ])
      expect(withoutRoutingHeader(text)).toBe(text);
  });
});

describe("naming a background result", () => {
  const completion = (status: string, tool = "bash_unrestricted") =>
    `<tool-completion tool_call_id="call-1" tool_name="${tool}" status="${status}">\n  <result>ok</result>\n</tool-completion>`;
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
      summary: { title: "slow worker (engineer)", agentId: "engineer" },
      worker: null,
    } as unknown as Reached;
    expect(backgroundName(completion("completed"), worker, reached)).toEqual({
      subject: "slow worker (engineer)",
      outcome: "finished",
      worker: { sessionId: "session-worker", agentId: "engineer" },
    });
    expect(backgroundName(completion("failed"), worker)).toEqual({
      subject: "engineer",
      outcome: "failed",
      worker: { sessionId: "session-from-call", agentId: null },
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

  it("says what an input that names no call is, by its words", () => {
    const wake =
      "Review the new background completion results and continue the task if needed.";
    expect(backgroundName(wake, null)).toBeNull();
    expect(backgroundText(null, wake)).toBe("Asked to review background results");
    expect(backgroundText(null, "\n  <agent-notification>done  \nmore")).toBe(
      "<agent-notification>done",
    );
    const long = "x".repeat(120);
    expect(backgroundText(null, long)).toBe(`${"x".repeat(79)}…`);
    expect(backgroundText(null, "x".repeat(80))).toBe("x".repeat(80));
    expect(backgroundText(null, "  \n ")).toBe("Background result");
    expect(backgroundText(null)).toBe("Background result");
  });

  it("counts the completions a run folds", () => {
    const result = (content: string): BackgroundResult =>
      ({ item: { content }, call: null, prompts: [] }) as unknown as BackgroundResult;
    const other = result("an input that names no call");
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
  const routed = (
    session: string,
    node = "did:key:z6MkSend..fier",
    cause = "request-1",
  ) =>
    `[message agent:engineer session:${session} node:${node} · at:2026-11-01T13:53:39Z · cause:${cause}]\n\nhello`;

  it("finds the sender session the origin names, under the sender node first", () => {
    expect(
      senderSession(fleet, origin({ senderSessionId: "session-reviewer" }), null),
    ).toBe(reviewer);
    expect(
      senderSession(
        fleet,
        origin({ senderNodeDid: null, senderSessionId: "session-elsewhere" }),
        null,
      ),
    ).toBe(elsewhere);
    expect(
      senderSession(fleet, origin({ senderSessionId: "unlisted" }), null),
    ).toBeNull();
  });

  it("reads the routing line only when it agrees with the authenticated sender", () => {
    expect(senderSession(fleet, origin(), routed("session-reviewer"))).toBe(reviewer);
    expect(senderSession(fleet, origin(), routed("session-elsewhere"))).toBeNull();
    expect(
      senderSession(
        fleet,
        origin(),
        routed("session-reviewer", "did:key:z6MkSome..0000"),
      ),
    ).toBeNull();
    expect(
      senderSession(
        fleet,
        origin(),
        routed("session-reviewer", undefined, "request-2"),
      ),
    ).toBeNull();
    expect(
      senderSession(fleet, origin({ senderNodeDid: null }), routed("session-reviewer")),
    ).toBeNull();
    expect(
      senderSession(fleet, origin(), `hello\n${routed("session-reviewer")}`),
    ).toBeNull();
  });
});
