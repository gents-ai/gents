import { describe, expect, it } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import { createChatStore } from "../src/hooks/chatStore";
import { createClientStore } from "../src/hooks/clientStore";
import { createFleetStore } from "../src/hooks/fleetStore";
import { createSelectionStore } from "../src/hooks/selectionStore";
import { createSessionStore, writeSession } from "../src/hooks/sessionStore";
import {
  projectShell,
  projectionInputsOf,
  type ShellStores,
} from "../src/hooks/shellProjection";
import { createShellView } from "../src/hooks/shellView";
import { shellStores } from "./shell-fixture";

/* the projection an action reads is the stores' state when it runs, with
   no copy kept in step during render */
describe("the shell projection from the stores", () => {
  it("tracks the request a send is awaiting, as the stores hold it now", () => {
    const stores: ShellStores = {
      selection: createSelectionStore({ nodeDid: "node", sessionId: "s1" }),
      session: createSessionStore(),
      fleet: createFleetStore(),
      client: createClientStore(),
      chat: createChatStore(),
    };
    const now = () => projectShell(projectionInputsOf(stores)).trackedRequestId;
    expect(now()).toBeNull();
    stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: "node",
        sessionId: "s1",
        requestId: "r1",
      },
    });
    expect(now()).toBe("r1");
    /* another session selected: its own workflow is not this one's */
    stores.selection.setState({ sessionId: "s2" });
    expect(now()).toBeNull();
  });
});

describe("the shell view", () => {
  const live = (content: string) =>
    ({
      sessionId: "s1",
      nodeDid: "node",
      agentId: null,
      turnState: "running",
      latestRequestId: "r1",
      pendingTurn: null,
      queuedTurns: [],
      foldedInputs: [],
      hydration: null,
      timelineItems: [{ kind: "liveAssistant", itemKey: "live", content }],
    }) as unknown as DesktopSessionSnapshot;

  it("notifies no one for a streamed chunk", () => {
    const stores = shellStores({ selection: { nodeDid: "node", sessionId: "s1" } });
    const view = createShellView(stores);
    writeSession(stores.session, live("Hel"));
    const before = view.getState();
    let notified = 0;
    view.subscribe(() => (notified += 1));
    writeSession(stores.session, live("Hello"));
    expect(notified).toBe(0);
    expect(view.getState()).toBe(before);
  });

  it("is in step with a write before the writer reads it", () => {
    const stores = shellStores({ selection: { nodeDid: "node", sessionId: "s1" } });
    const view = createShellView(stores);
    const keyBefore = view.getState().draftKey;
    stores.selection.setState({ sessionId: "s2" });
    expect(view.getState().draftKey).not.toBe(keyBefore);
    stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: "node",
        sessionId: "s2",
        requestId: "r2",
      },
    });
    expect(view.getState().trackedRequestId).toBe("r2");
  });
});
