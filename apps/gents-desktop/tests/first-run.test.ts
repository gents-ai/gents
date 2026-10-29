import { describe, expect, it } from "vitest";

import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  inferenceIsConfigured,
  hasSavedRemote,
  isLocalNode,
  needsFirstRunSetup,
  nodeSetUp,
  projectFirstRunPhase,
  shouldRebindSetupDefault,
} from "../src/ui/lib/firstRun";

describe("first-run inference gate", () => {
  it("waits through a cold partial replica, then accepts authoritative config", () => {
    const unresolved = projectFirstRunPhase("unknown", true, "configured", true, false);
    expect(unresolved).toBe("unknown");
    expect(projectFirstRunPhase(unresolved, true, "configured", false, true)).toBe(
      "done",
    );
  });

  it("allows setup for an existing but uninitialized home", () => {
    expect(projectFirstRunPhase("unknown", true, "absent", true, false)).toBe("active");
  });

  it("reopens a saved remote setup even while its route is not ready", () => {
    const snapshot = {
      bootstrap: {
        ...bootstrap,
        initConfigStatus: "absent",
        initNodeDid: null,
        savedPeers: [{ ...deployment, source: "enrollment" }],
      },
      client: null,
    } as Parameters<typeof hasSavedRemote>[0];
    expect(hasSavedRemote(snapshot)).toBe(true);
    expect(projectFirstRunPhase("unknown", true, "absent", true, false, true)).toBe(
      "done",
    );
    expect(projectFirstRunPhase("unknown", false, "absent", true, false, true)).toBe(
      "unknown",
    );
    expect(projectFirstRunPhase("unknown", true, "configured", true, true, true)).toBe(
      "active",
    );
  });

  it("does not treat a local or unrelated route as a saved remote setup", () => {
    const snapshot = {
      bootstrap: {
        ...bootstrap,
        initNodeDid: deployment.nodeDid,
        savedPeers: [{ ...deployment, source: "enrollment" }],
      },
      client: null,
    } as Parameters<typeof hasSavedRemote>[0];
    expect(hasSavedRemote(snapshot)).toBe(false);
  });

  it("keeps setup unresolved when an existing init config cannot be decoded", () => {
    expect(projectFirstRunPhase("unknown", true, "unreadable", true, false)).toBe(
      "unknown",
    );
    expect(projectFirstRunPhase("unknown", true, null, true, false)).toBe("unknown");
  });

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
