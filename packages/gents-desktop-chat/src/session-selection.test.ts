import { describe, expect, it } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { sessionBelongsToBehavior } from "./session-selection.js";

function session(behaviorId?: string | null): SessionSummary {
  return {
    sessionId: "session",
    behaviorId,
    messageCount: 0,
    toolCallCount: 0,
  };
}

describe("session behavior selection", () => {
  it("matches persisted behavior ids exactly", () => {
    expect(
      sessionBelongsToBehavior(session("session-classifier"), "default"),
    ).toBe(false);
    expect(
      sessionBelongsToBehavior(
        session("session-classifier"),
        "session-classifier",
      ),
    ).toBe(true);
  });

  it("does not assign an unbound session to a behavior", () => {
    expect(sessionBelongsToBehavior(session(null), "default")).toBe(false);
  });
});
