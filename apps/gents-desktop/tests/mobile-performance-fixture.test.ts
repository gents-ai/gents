import { describe, expect, it } from "vitest";

import {
  MOBILE_PERFORMANCE_FIXTURE,
  createDesktopUiHarness,
} from "./ui-harness/desktopHarness";
import { sessionLiveDeltaRequest } from "../src/hooks/desktopShellRuntime";

function serializedBytes(value: unknown) {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

describe("mobile performance fixture structural budgets", () => {
  it("keeps the durable session-index fixture shape explicit and bounded", async () => {
    const harness = createDesktopUiHarness({ scenario: "mobile-performance" });
    const snapshot = await harness.adapter.fetchDesktopSnapshot();
    const conversations = snapshot.client?.deployments[0]?.conversations ?? [];

    expect(conversations).toHaveLength(MOBILE_PERFORMANCE_FIXTURE.sessionIndexCount);
    expect(serializedBytes(snapshot)).toBeLessThanOrEqual(512 * 1024);

    const shortSession = await harness.adapter.fetchSessionSnapshot(
      "session-intro",
      "did:key:z6MkBombadilAgent",
      null,
    );
    expect(shortSession?.timelineItems).toHaveLength(
      MOBILE_PERFORMANCE_FIXTURE.shortSessionTimelineItems,
    );
  });

  it("keeps the comparable large-session payload within its initial evidence ceiling", async () => {
    const harness = createDesktopUiHarness({ scenario: "mobile-performance" });
    const session = await harness.adapter.fetchSessionSnapshot(
      "session-large",
      "did:key:z6MkBombadilAgent",
      null,
    );

    expect(session?.timelineItems).toHaveLength(
      MOBILE_PERFORMANCE_FIXTURE.largeSessionTimelineItems,
    );
    // This guards fixture and projection growth; it does not bless full-session
    // bridge materialization as the eventual product budget.
    expect(serializedBytes(session)).toBeLessThanOrEqual(256 * 1024);
  });

  it("bounds bridge pages and advances the older cursor without overlap", async () => {
    const harness = createDesktopUiHarness({ scenario: "mobile-performance" });
    const tip = await harness.adapter.fetchSessionSnapshot(
      "session-large",
      "did:key:z6MkBombadilAgent",
      null,
      { limit: MOBILE_PERFORMANCE_FIXTURE.transcriptPageSize },
    );
    expect(tip?.timelineItems).toHaveLength(40);
    expect(tip?.timelinePage).toMatchObject({
      totalItems: MOBILE_PERFORMANCE_FIXTURE.largeSessionTimelineItems,
      pageItems: 40,
      hasOlder: true,
      hasNewer: false,
    });
    expect(serializedBytes(tip)).toBeLessThanOrEqual(16 * 1024);

    const older = await harness.adapter.fetchSessionSnapshot(
      "session-large",
      "did:key:z6MkBombadilAgent",
      null,
      {
        limit: MOBILE_PERFORMANCE_FIXTURE.transcriptPageSize,
        beforeItemKey: tip?.timelinePage?.oldestItemKey,
      },
    );
    expect(older?.timelineItems).toHaveLength(40);
    expect(older?.timelinePage?.hasNewer).toBe(true);
    expect(
      older?.timelineItems.some((olderItem) =>
        tip?.timelineItems.some((tipItem) => tipItem.itemKey === olderItem.itemKey),
      ),
    ).toBe(false);
    expect(serializedBytes(older)).toBeLessThanOrEqual(16 * 1024);
  });

  it("streams a verified suffix instead of another session snapshot", async () => {
    const harness = createDesktopUiHarness({ scenario: "mobile-performance" });
    const tip = await harness.adapter.fetchSessionSnapshot(
      "session-large",
      "did:key:z6MkBombadilAgent",
      "large-request-live",
      { limit: MOBILE_PERFORMANCE_FIXTURE.transcriptPageSize },
    );
    expect(tip).not.toBeNull();
    const request = sessionLiveDeltaRequest(tip!, "large-request-live");
    expect(request).not.toBeNull();

    harness.performance!.streamUpdate();
    const delta = await harness.adapter.fetchSessionLiveDelta!(request!);
    expect(delta).toMatchObject({
      outcome: "delta",
      requestId: "large-request-live",
      content: { mode: "append" },
    });
    expect(delta?.content?.value).toContain("stream-chunk-1");
    expect(serializedBytes(delta)).toBeLessThanOrEqual(2 * 1024);
  });
});
