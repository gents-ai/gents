import { describe, expect, it } from "vitest";
import { activityStatus } from "../src/ui/screens/activity-status";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

describe("projected execution activity", () => {
  it("shows provider state and gives stop and active tools precedence", () => {
    expect(activityStatus([], false, "retrying")).toBe("Retrying connection");
    expect(activityStatus([], false, "unavailable")).toBe("Provider unavailable");
    expect(activityStatus([], false, "unknown")).toBe("Waiting for an update");
    expect(activityStatus([], false, "queued")).toBe("Waiting to start");
    expect(activityStatus([], true, "retrying")).toBe("Stopping…");
    const group = {
      kind: "toolGroup",
      tools: [{ statusKind: "running" }],
    } as unknown as RenderedTimelineItem;
    expect(activityStatus([group], false, "retrying")).toBeNull();
  });
});
