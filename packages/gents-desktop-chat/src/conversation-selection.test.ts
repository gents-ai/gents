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
        "default",
      ),
    ).toBe(false);
    expect(
      conversationBelongsToBehavior(
        conversation("session-classifier"),
        "session-classifier",
        "default",
      ),
    ).toBe(true);
  });

  it("places legacy unscoped conversations only under the default behavior", () => {
    expect(
      conversationBelongsToBehavior(conversation(null), "default", "default"),
    ).toBe(true);
    expect(
      conversationBelongsToBehavior(
        conversation(null),
        "session-classifier",
        "default",
      ),
    ).toBe(false);
  });
});
