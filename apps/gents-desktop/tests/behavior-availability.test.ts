import { describe, expect, it } from "vitest";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { selectedBehaviorUnavailableHint } from "../src/lib/behaviorAvailability";

function deployment(probeStatus: string | null, enabled = true): DeploymentView {
  return {
    defaultBehaviorId: "default",
    behaviors: [
      {
        behaviorId: "default",
        displayName: "Default",
        backendId: "workstation-2",
        enabled: true,
        isDefault: true,
      },
    ],
    inferenceBackends: [
      {
        backendId: "workstation-2",
        name: "Workstation 2",
        enabled,
        probeStatus,
      },
    ],
  } as DeploymentView;
}

describe("selectedBehaviorUnavailableHint", () => {
  it("treats persisted probe status as advisory and blocks disabled config", () => {
    expect(selectedBehaviorUnavailableHint(deployment("healthy"), null)).toBeNull();
    expect(selectedBehaviorUnavailableHint(deployment("unknown"), null)).toBeNull();
    expect(selectedBehaviorUnavailableHint(deployment("unhealthy"), null)).toBeNull();
    expect(selectedBehaviorUnavailableHint(deployment("unknown", false), null)).toBe(
      "Backend “Workstation 2” is disabled",
    );
  });
});
