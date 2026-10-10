import { describe, expect, it } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { sessionBelongsToAgent } from "./session-selection.js";

function session(agentId?: string | null): SessionSummary {
  return {
    sessionId: "session",
    agentId,
    messageCount: 0,
    toolCallCount: 0,
  };
}

describe("session agent selection", () => {
  it("matches persisted agent ids exactly", () => {
    expect(
      sessionBelongsToAgent(session("session-classifier"), "default"),
    ).toBe(false);
    expect(
      sessionBelongsToAgent(
        session("session-classifier"),
        "session-classifier",
      ),
    ).toBe(true);
  });

  it("does not assign an unbound session to an agent", () => {
    expect(sessionBelongsToAgent(session(null), "default")).toBe(false);
  });
});
