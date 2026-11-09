import { describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import {
  completedReconnectSession,
  findAssistantResponseMarker,
} from "../src/lib/nativeSimulatorE2eDom";

const session = (changes: Partial<SessionSummary> = {}): SessionSummary =>
  ({
    sessionId: "new",
    agentDid: "agent",
    requesterDid: "client",
    latestRequestId: "request",
    turnState: "completed",
    ...changes,
  }) as SessionSummary;

describe("real reconnect inference evidence", () => {
  it("requires a new terminal session owned by the held client and selected agent", () => {
    const previous = new Set(["old"]);
    expect(
      completedReconnectSession([session()], previous, "agent", "client")
        ?.latestRequestId,
    ).toBe("request");
    for (const changes of [
      { sessionId: "old" },
      { requesterDid: "other" },
      { agentDid: "other" },
      { latestRequestId: null },
      { turnState: "running" },
      { turnState: "failed" },
    ]) {
      expect(
        completedReconnectSession([session(changes)], previous, "agent", "client"),
      ).toBeNull();
    }
    expect(() =>
      completedReconnectSession(
        [session(), session({ sessionId: "another" })],
        previous,
        "agent",
        "client",
      ),
    ).toThrow(/Ambiguous/);
  });

  it("accepts the marker only from an assistant message", () => {
    document.body.innerHTML = '<div data-slot="user-message">MARKER</div>';
    expect(findAssistantResponseMarker(document, "MARKER")).toBeNull();
    document.body.innerHTML += '<div data-slot="assistant-message">MARKER</div>';
    expect(findAssistantResponseMarker(document, "MARKER")).not.toBeNull();
  });
});
