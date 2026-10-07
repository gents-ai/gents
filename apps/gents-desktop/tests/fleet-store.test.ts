import { describe, expect, it } from "vitest";

import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";
import {
  applyFleetSnapshot,
  createFleetStore,
  sessionKeyOf,
} from "../src/hooks/fleetStore";

const session = (
  agentDid: string,
  sessionId: string,
  startedBy?: string,
  title = sessionId,
) => ({
  agentDid,
  sessionId,
  requesterDid: "me",
  title,
  startedBy: startedBy
    ? { agentDid: "a", sessionId: startedBy, requesterDid: "me" }
    : null,
});

const read = (parentTitle = "parent") =>
  ({
    bootstrap: { initAgentDid: "a" },
    client: {
      deployments: [
        {
          agentDid: "a",
          source: "enrollment",
          label: "home",
          behaviors: [{ behaviorId: "a:default" }],
          sessions: [
            session("a", "parent", undefined, parentTitle),
            session("a", "local-worker", "parent"),
          ],
          mailboxItems: [{ itemId: "m1", status: "open" }],
        },
        {
          agentDid: "b",
          source: "enrollment",
          label: "remote",
          behaviors: [],
          sessions: [session("b", "remote-worker", "parent"), session("b", "alone")],
          mailboxItems: [],
        },
      ],
    },
  }) as unknown as DesktopClientSnapshot;

const parentKey = sessionKeyOf(session("a", "parent"));

describe("the fleet store", () => {
  it("indexes lineage across nodes", () => {
    const store = createFleetStore();
    applyFleetSnapshot(store, read());
    const state = store.getState();
    expect(state.nodeKeys).toEqual(["a", "b"]);
    expect(state.workersOf[parentKey]?.map((s) => s.sessionId)).toEqual([
      "local-worker",
      "remote-worker",
    ]);
    expect(state.parentOf[sessionKeyOf(session("b", "remote-worker"))]?.sessionId).toBe(
      "parent",
    );
    expect(state.nodes.a).not.toHaveProperty("sessions");
    expect(state.bySessionId.alone?.agentDid).toBe("b");
  });

  it("keeps every object when a read says nothing new", () => {
    const store = createFleetStore();
    applyFleetSnapshot(store, read());
    const before = store.getState();
    applyFleetSnapshot(store, read());
    /* the state itself, so subscribers are not notified at all */
    expect(store.getState()).toBe(before);
    for (const key of Object.keys(before) as (keyof typeof before)[])
      expect(store.getState()[key]).toBe(before[key]);
  });

  it("replaces only what changed when one session moves", () => {
    const store = createFleetStore();
    applyFleetSnapshot(store, read());
    const before = store.getState();
    applyFleetSnapshot(store, read("renamed"));
    const after = store.getState();
    expect(after.sessions[parentKey]).not.toBe(before.sessions[parentKey]);
    expect(after.sessions[parentKey]?.title).toBe("renamed");
    expect(after.nodes).toBe(before.nodes);
    expect(after.sessionsOf.b).toBe(before.sessionsOf.b);
    expect(after.mailboxOf).toBe(before.mailboxOf);
    /* the workers list holds the same worker objects, so it is kept */
    expect(after.workersOf[parentKey]).toBe(before.workersOf[parentKey]);
  });
});
