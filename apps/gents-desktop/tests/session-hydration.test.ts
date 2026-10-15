import { describe, expect, it } from "vitest";

import type { SessionHydrationView } from "@source-inc/gents-desktop-client";
import {
  sessionHydrationLabel,
  sessionHydrationNeedsRetry,
  visibleSessionHydration,
  type VisibleSessionHydration,
} from "../src/lib/sessionHydration";

function hydration(
  overrides: Partial<SessionHydrationView> = {},
): SessionHydrationView {
  return {
    sessionId: "session-1",
    nodeDid: "did:test:agent",
    phase: "serving",
    mergedCount: 4,
    coveredCount: 4,
    servedCount: 11,
    ...overrides,
  };
}

/* a hydration the screen shows: one of the phases it names */
const visible = (overrides: Partial<VisibleSessionHydration> = {}) =>
  ({ ...hydration(), phase: "serving", ...overrides }) as VisibleSessionHydration;

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
    const unreadable = visibleSessionHydration(
      hydration({ phase: "unreadable" }),
      "session-1",
    );
    expect(unreadable?.phase).toBe("unreadable");
    expect(unreadable && sessionHydrationNeedsRetry(unreadable)).toBe(false);
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
        hydration({ nodeDid: "" }),
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
        visible({ phase: "requested", mergedCount: 0, servedCount: null }),
      ),
    ).toBe("Fetching session history");
    expect(sessionHydrationLabel(visible())).toBe("Fetching session history · 4 of 11");
    expect(
      sessionHydrationLabel(
        visible({ mergedCount: 124, coveredCount: 47, servedCount: 47 }),
      ),
    ).toBe("Fetching session history · 47 of 47");
    expect(
      sessionHydrationLabel(
        visible({ servedCount: null, mergedCount: 3, coveredCount: 3 }),
      ),
    ).toBe("Fetching session history · 3 documents so far");
    expect(
      sessionHydrationLabel(
        visible({
          phase: "complete",
          mergedCount: 124,
          coveredCount: 47,
          servedCount: 47,
        }),
      ),
    ).toBe("Session history loaded · 47 of 47");
    expect(sessionHydrationLabel(visible({ phase: "failed" }))).toBe(
      "Couldn't fetch the rest of this session",
    );
    expect(sessionHydrationNeedsRetry(visible({ phase: "failed" }))).toBe(true);
    expect(sessionHydrationNeedsRetry(visible())).toBe(false);
  });
});
