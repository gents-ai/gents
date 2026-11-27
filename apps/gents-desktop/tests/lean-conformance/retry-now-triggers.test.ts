import { describe, expect, it } from "vitest";

import { asksRetryNow, type AppEvent } from "../../src/hooks/clientObservation";
import { loadLeanFamilyCases } from "./leanGenerator";

type LeanEvent = "shown_again" | "hidden" | "focused" | "online" | "network_changed";

const desktopEvent: Record<LeanEvent, AppEvent> = {
  shown_again: "shown-again",
  hidden: "hidden",
  focused: "focused",
  online: "online",
  network_changed: "network-changed",
};

describe("generated retry-now triggers", () => {
  it("drives asksRetryNow with every Lean app event", async () => {
    const cases = await loadLeanFamilyCases<{
      input: { app_event: LeanEvent };
      expected: boolean;
    }>("families::observer_resync_asks_retry_now::cases()");
    expect(cases).toHaveLength(5);
    for (const { input, expected } of cases)
      expect(asksRetryNow(desktopEvent[input.app_event]), input.app_event).toBe(
        expected,
      );
  }, 600_000);
});
