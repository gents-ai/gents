import { describe, expect, it } from "vitest";

import { useSelectionReconcile } from "../src/hooks/useDesktopRuntime";
import { useFollowRoute } from "../src/ui/hooks/useFollowRoute";
import { node, renderIn, testApp } from "./app-fixture";

const session = (agentDid: string, sessionId: string, behaviorId = "default") => ({
  agentDid,
  sessionId,
  behaviorId,
});

describe("startup under a route", () => {
  it("keeps the node and behavior a session route selected on the first ready render", () => {
    const app = testApp({
      deployments: [
        node({ agentDid: "did:key:a", sessions: [session("did:key:a", "a-1")] }),
        node({ agentDid: "did:key:b", sessions: [session("did:key:b", "b-1", "ops")] }),
      ],
    });
    /* as the app mounts them: the runtime at the root, the route below it */
    function Route() {
      useFollowRoute({ name: "session", sessionId: "b-1" });
      return null;
    }
    function Root() {
      useSelectionReconcile(app);
      return <Route />;
    }
    renderIn(app, <Root />);
    expect(app.stores.selection.getState()).toMatchObject({
      agentDid: "did:key:b",
      sessionId: "b-1",
      /* the fixture node's default is "default"; the session was held under "ops" */
      behaviorId: "ops",
    });
  });
});
