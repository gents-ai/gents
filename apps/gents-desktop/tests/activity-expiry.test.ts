import { describe, expect, it } from "vitest";
import { activityExpiry } from "../src/hooks/activityExpiry";

describe("execution activity expiry", () => {
  const session = {
    sessionId: "s",
    nodeDid: "node",
    latestRequestId: "r",
    executionActivityExpiresAt: 1500,
  };
  it("schedules only the selected session and clamps expired observations", () => {
    expect(activityExpiry(session, "s", "node", 1000)?.delay).toBe(500);
    expect(activityExpiry(session, "s", "node", 1600)?.delay).toBe(0);
    expect(activityExpiry(session, "other", "node", 1000)).toBeNull();
    expect(activityExpiry(session, "s", "other", 1000)).toBeNull();
    expect(
      activityExpiry(
        { ...session, executionActivityExpiresAt: null },
        "s",
        "node",
        1000,
      ),
    ).toBeNull();
  });
});
