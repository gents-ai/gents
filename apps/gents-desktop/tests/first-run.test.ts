import { describe, expect, it } from "vitest";

import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  inferenceIsConfigured,
  isLocalNode,
  needsFirstRunSetup,
  nodeSetUp,
  shouldRebindSetupDefault,
} from "../src/ui/lib/firstRun";

describe("first-run inference gate", () => {
  it("recognizes the initialized local node after background enrollment changes its route source", () => {
    const paired = { ...deployment, source: "enrollment" };
    expect(isLocalNode(paired, deployment.nodeDid)).toBe(true);
    expect(isLocalNode(paired, "did:another-home")).toBe(false);
    expect(
      needsFirstRunSetup({
        bootstrap: { ...bootstrap, initNodeDid: deployment.nodeDid },
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
            authKind: "node_oauth",
          },
        ],
      }),
    ).toBe(false);
  });

  it("treats a local placeholder backend as unfinished", () => {
    expect(isLocalNode(deployment)).toBe(true);
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
    const defaultAgentId = deployment.node.defaultAgentId;
    const generatedBackendId = `${deployment.nodeDid}:backend`;
    const generated = {
      ...deployment,
      agents: deployment.agents.map((agent) =>
        agent.agentId === defaultAgentId
          ? { ...agent, inferenceProfileId: "profile-generated" }
          : agent,
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

describe("the node first-run setup configured", () => {
  const remote = { ...deployment, nodeDid: "did:key:remote", source: "enrollment" };
  const home = { ...deployment, nodeDid: "did:key:home", source: "enrollment" };
  const read = (deployments: (typeof deployment)[]) =>
    ({
      bootstrap: { ...bootstrap, initNodeDid: "did:key:home" },
      client: { deployments },
    }) as Parameters<typeof nodeSetUp>[0];

  it("is the one this machine runs, even when a remote node is listed first", () => {
    expect(nodeSetUp(read([remote, home]))?.nodeDid).toBe("did:key:home");
  });

  it("is the first listed when this machine runs none, and none without nodes", () => {
    expect(nodeSetUp(read([remote]))?.nodeDid).toBe("did:key:remote");
    expect(nodeSetUp(read([]))).toBeNull();
  });
});
