import { describe, expect, it } from "vitest";

import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  inferenceIsConfigured,
  isLocalAgent,
  needsFirstRunSetup,
  shouldRebindSetupDefault,
} from "../src/ui/lib/firstRun";

describe("first-run inference gate", () => {
  it("recognizes the initialized local principal after background enrollment changes its route source", () => {
    const paired = { ...deployment, source: "enrollment" };
    expect(isLocalAgent(paired, deployment.agentDid)).toBe(true);
    expect(isLocalAgent(paired, "did:another-home")).toBe(false);
    expect(
      needsFirstRunSetup({
        bootstrap: { ...bootstrap, initAgentDid: deployment.agentDid },
        client: { deployments: [paired] },
      } as Parameters<typeof needsFirstRunSetup>[0]),
    ).toBe(true);
  });
  it("does not finish setup just because an unrelated OAuth backend exists", () => {
    expect(
      inferenceIsConfigured({
        ...deployment,
        inferenceBackends: [
          ...deployment.inferenceBackends,
          {
            ...deployment.inferenceBackends[0]!,
            backendId: "codex-unbound",
            authKind: "principal_oauth",
          },
        ],
      }),
    ).toBe(false);
  });

  it("treats a local placeholder backend as unfinished", () => {
    expect(isLocalAgent(deployment)).toBe(true);
    expect(inferenceIsConfigured(deployment)).toBe(false);
    expect(
      needsFirstRunSetup({
        bootstrap,
        client: null,
      }),
    ).toBe(true);
  });

  it("treats a keyed or probed backend as finished", () => {
    expect(
      inferenceIsConfigured({
        ...deployment,
        inferenceBackends: deployment.inferenceBackends.map((backend) => ({
          ...backend,
          apiKeyConfigured: true,
        })),
      }),
    ).toBe(true);
    expect(
      inferenceIsConfigured({
        ...deployment,
        inferenceBackends: [
          {
            ...deployment.inferenceBackends[0]!,
            apiKeyConfigured: false,
            authKind: "unauthenticated",
            probeStatus: "healthy",
          },
        ],
      }),
    ).toBe(true);
  });

  it("replaces only init's generated default when another provider exists", () => {
    const defaultBehaviorId = deployment.agentPrincipal.defaultBehaviorId;
    const generatedBackendId = `${deployment.agentDid}:backend`;
    const generated = {
      ...deployment,
      behaviors: deployment.behaviors.map((behavior) =>
        behavior.behaviorId === defaultBehaviorId
          ? { ...behavior, inferenceProfileId: "profile-generated" }
          : behavior,
      ),
      inferenceProfiles: [
        {
          ...deployment.inferenceProfiles[0]!,
          profile_id: "profile-generated",
          backend_id: generatedBackendId,
        },
      ],
    };

    expect(shouldRebindSetupDefault(generated, true)).toBe(true);
    expect(shouldRebindSetupDefault(deployment, true)).toBe(false);
    expect(shouldRebindSetupDefault(deployment, false)).toBe(true);
  });
});
