import { describe, expect, it } from "vitest";

import { parsePeerConnectionJson } from "@source-inc/gents-desktop-fleet";

describe("parsePeerConnectionJson", () => {
  it("accepts the desktop connection shape", () => {
    expect(
      parsePeerConnectionJson(
        JSON.stringify({
          label: "api-gateway",
          agentDid: "did:key:z6MkAgent",
          addr: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooAgent",
        }),
      ),
    ).toEqual({
      label: "api-gateway",
      agentDid: "did:key:z6MkAgent",
      addr: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooAgent",
    });
  });

  it("accepts Gents status output", () => {
    expect(
      parsePeerConnectionJson(
        JSON.stringify({
          agent_did: "did:key:z6MkInfraApi",
          desktop_graphql: "http://100.73.235.38:9181/api/v0/graphql",
          runtime_state: {
            agent_name: "infra-api",
          },
          p2p: {
            p2p_shareable_address: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooInfra",
          },
        }),
      ),
    ).toEqual({
      label: "infra-api",
      agentDid: "did:key:z6MkInfraApi",
      addr: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooInfra",
      graphql: "http://100.73.235.38:9181/api/v0/graphql",
    });
  });

  it("accepts Gents server output", () => {
    expect(
      parsePeerConnectionJson(
        JSON.stringify({
          agent_name: "worker-a",
          agent_did: "did:key:z6MkWorkerA",
          p2p_listen_addresses: ["/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker"],
        }),
      ),
    ).toEqual({
      label: "worker-a",
      agentDid: "did:key:z6MkWorkerA",
      addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
    });
  });

  it("can extract a JSON object from copied terminal text", () => {
    expect(
      parsePeerConnectionJson(`
        startup logs
        {"agent_did":"did:key:z6MkOps","agent_name":"ops","p2p_shareable_address":"endpoint://ops"}
        trailing logs
      `),
    ).toEqual({
      label: "ops",
      agentDid: "did:key:z6MkOps",
      addr: "endpoint://ops",
    });
  });

  it("accepts any non-empty concrete agent DID", () => {
    expect(
      parsePeerConnectionJson(
        JSON.stringify({
          agent_did: "did:test:infra-api",
          p2p_shareable_address: "endpoint://ops",
        }),
      ),
    ).toEqual({
      label: "infra-api",
      agentDid: "did:test:infra-api",
      addr: "endpoint://ops",
    });
  });

  it("explains when a runtime cannot supply a P2P connection", () => {
    expect(() =>
      parsePeerConnectionJson(
        JSON.stringify({
          agent_did: "did:key:z6MkLocalOnly",
          p2p_transport: "none",
          p2p: {
            enabled: false,
            p2p_listen_addresses: [],
          },
        }),
      ),
    ).toThrow(
      "This runtime has P2P disabled. Restart it with --p2p-transport iroh and fetch again.",
    );
  });
});
