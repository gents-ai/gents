import { describe, expect, it } from "vitest";

import { parsePeerConnectionJson } from "./peerConnectionImport.js";

describe("parsePeerConnectionJson", () => {
  it("preserves the enabled default behavior from a Gents status response", () => {
    const request = parsePeerConnectionJson(
      JSON.stringify({
        agent_did: "did:key:z6MkAmy",
        agent_name: "Amy",
        graphql: "http://127.0.0.1:9191/api/v0/graphql",
        p2p_shareable_address: "endpoint-amy",
        behaviors: [
          {
            behavior_id: "session-classifier",
            enabled: true,
          },
          {
            behavior_id: "default",
            enabled: true,
          },
        ],
      }),
    );

    expect(request).toMatchObject({
      label: "Amy",
      agentDid: "did:key:z6MkAmy",
      addr: "endpoint-amy",
      graphql: "http://127.0.0.1:9191/api/v0/graphql",
      defaultBehaviorId: "default",
    });
  });

  it("does not import a disabled default behavior", () => {
    const request = parsePeerConnectionJson(
      JSON.stringify({
        agent_did: "did:key:z6MkAmy",
        agent_name: "Amy",
        p2p_shareable_address: "endpoint-amy",
        behaviors: [
          {
            behavior_id: "default",
            enabled: false,
          },
        ],
      }),
    );

    expect(request.defaultBehaviorId).toBeUndefined();
  });
});
