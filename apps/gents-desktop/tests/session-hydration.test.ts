import { describe, expect, it } from "vitest";

import type { SessionHydrationView } from "@source-inc/gents-desktop-client";
import {
  sessionHydrationLabel,
  sessionHydrationNeedsRetry,
  visibleSessionHydration,
} from "../src/lib/sessionHydration";

function hydration(
  overrides: Partial<SessionHydrationView> = {},
): SessionHydrationView {
  return {
    sessionId: "session-1",
    agentDid: "did:test:agent",
    phase: "serving",
    mergedCount: 4,
    coveredCount: 4,
    servedCount: 11,
    ...overrides,
  };
}

describe("visibleSessionHydration", () => {
  it("keeps requested, serving, complete, and failed for the selected session", () => {
    expect(
      visibleSessionHydration(hydration({ phase: "requested" }), "session-1")?.phase,
    ).toBe("requested");
    expect(visibleSessionHydration(hydration(), "session-1")?.phase).toBe("serving");
    expect(
      visibleSessionHydration(hydration({ phase: "complete" }), "session-1")?.phase,
    ).toBe("complete");
    expect(
      visibleSessionHydration(hydration({ phase: "failed" }), "session-1")?.phase,
    ).toBe("failed");
  });

  it("suppresses idle, empty complete, and other-session updates", () => {
    expect(
      visibleSessionHydration(hydration({ phase: "idle" }), "session-1"),
    ).toBeNull();
    expect(
      visibleSessionHydration(
        hydration({ phase: "complete", mergedCount: 0, servedCount: 0 }),
        "session-1",
      ),
    ).toBeNull();
    expect(visibleSessionHydration(hydration(), "session-2")).toBeNull();
    expect(visibleSessionHydration(hydration(), "session-1", "did:other")).toBeNull();
    expect(
      visibleSessionHydration(
        hydration({ agentDid: "" }),
        "session-1",
        "did:test:agent",
      ),
    ).toBeNull();
  });
});

describe("sessionHydrationLabel", () => {
  it("names requested, N of M, complete, and failed states", () => {
    expect(
      sessionHydrationLabel(
        hydration({ phase: "requested", mergedCount: 0, servedCount: null }),
      ),
    ).toBe("Fetching session history");
    expect(sessionHydrationLabel(hydration())).toBe(
      "Fetching session history · 4 of 11",
    );
    expect(
      sessionHydrationLabel(
        hydration({ mergedCount: 124, coveredCount: 47, servedCount: 47 }),
      ),
    ).toBe("Fetching session history · 47 of 47");
    expect(
      sessionHydrationLabel(
        hydration({ servedCount: null, mergedCount: 3, coveredCount: 3 }),
      ),
    ).toBe("Fetching session history · 3 documents so far");
    expect(
      sessionHydrationLabel(
        hydration({
          phase: "complete",
          mergedCount: 124,
          coveredCount: 47,
          servedCount: 47,
        }),
      ),
    ).toBe("Session history loaded · 47 of 47");
    expect(sessionHydrationLabel(hydration({ phase: "failed" }))).toBe(
      "Couldn't fetch the rest of this session",
    );
    expect(sessionHydrationNeedsRetry(hydration({ phase: "failed" }))).toBe(true);
    expect(sessionHydrationNeedsRetry(hydration())).toBe(false);
  });
});
