import { describe, expect, it } from "vitest";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { selectedBehaviorUnavailableHint } from "../src/lib/behaviorAvailability";

function deployment(probeStatus: string | null): DeploymentView {
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
        enabled: true,
        probeStatus,
      },
    ],
  } as DeploymentView;
}

describe("selectedBehaviorUnavailableHint", () => {
  it("admits only a healthy selected behavior backend", () => {
    expect(selectedBehaviorUnavailableHint(deployment("healthy"), null)).toBeNull();
    expect(selectedBehaviorUnavailableHint(deployment("unknown"), null)).toBe(
      "Backend “Workstation 2” is still checking readiness",
    );
    expect(selectedBehaviorUnavailableHint(deployment("unhealthy"), null)).toBe(
      "Backend “Workstation 2” is unavailable (unhealthy)",
    );
  });
});
