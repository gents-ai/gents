import { describe, expect, it } from "vitest";

import { useFollowRoute } from "../src/ui/hooks/useFollowRoute";
import { node, renderIn, testApp } from "./app-fixture";

const session = (nodeDid: string, sessionId: string, agentId = "default") => ({
  nodeDid,
  sessionId,
  agentId,
});

describe("startup under a route", () => {
  it("keeps the node and agent a session route selected on the first ready render", () => {
    const app = testApp({
      deployments: [
        node({ nodeDid: "did:key:a", sessions: [session("did:key:a", "a-1")] }),
        node({ nodeDid: "did:key:b", sessions: [session("did:key:b", "b-1", "ops")] }),
      ],
    });
    function Route() {
      useFollowRoute({ name: "session", sessionId: "b-1" });
      return null;
    }
    renderIn(app, <Route />);
    expect(app.stores.selection.getState()).toMatchObject({
      nodeDid: "did:key:b",
      sessionId: "b-1",
      /* the fixture node's default is "default"; the session was held under "ops" */
      agentId: "ops",
    });
  });
});
