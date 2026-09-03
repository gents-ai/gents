import { describe, expect, it } from "vitest";

import type { ConversationSummary } from "@source-inc/gents-desktop-client";

import { conversationBelongsToBehavior } from "./conversation-selection.js";

function conversation(behaviorId?: string | null): ConversationSummary {
  return {
    sessionId: "session",
    behaviorId,
    messageCount: 0,
    toolCallCount: 0,
  };
}

describe("conversation behavior selection", () => {
  it("matches persisted behavior ids exactly", () => {
    expect(
      conversationBelongsToBehavior(
        conversation("session-classifier"),
        "default",
      ),
    ).toBe(false);
    expect(
      conversationBelongsToBehavior(
        conversation("session-classifier"),
        "session-classifier",
      ),
    ).toBe(true);
  });

  it("does not assign an unbound conversation to a behavior", () => {
    expect(conversationBelongsToBehavior(conversation(null), "default")).toBe(false);
  });
});
