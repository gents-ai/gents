import { describe, expect, it } from "vitest";

import type {
  DesktopClientSnapshot,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import {
  applyFleetSnapshot,
  createFleetStore,
  listedSession,
  nodeOrFirst,
  sessionKeyOf,
} from "../src/hooks/fleetStore";

const session = (
  nodeDid: string,
  sessionId: string,
  startedBy?: string,
  title = sessionId,
) => ({
  nodeDid,
  sessionId,
  requesterDid: "me",
  title,
  startedBy: startedBy
    ? { nodeDid: "a", sessionId: startedBy, requesterDid: "me" }
    : null,
});

const read = (parentTitle = "parent") =>
  ({
    bootstrap: { initNodeDid: "a" },
    client: {
      deployments: [
        {
          nodeDid: "a",
          source: "enrollment",
          label: "home",
          agents: [{ agentId: "a:default" }],
          sessions: [
            session("a", "parent", undefined, parentTitle),
            session("a", "local-worker", "parent"),
          ],
          mailboxItems: [{ itemId: "m1", status: "open" }],
        },
        {
          nodeDid: "b",
          source: "enrollment",
          label: "remote",
          agents: [],
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
    expect(state.bySessionId.alone?.nodeDid).toBe("b");
  });

  it("keeps every object when a read says nothing new", () => {
    const store = createFleetStore();
    applyFleetSnapshot(store, read());
    const before = store.getState();
    applyFleetSnapshot(store, read());
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
    expect(after.byNodeSessionId.b).toBe(before.byNodeSessionId.b);
    expect(after.mailboxOf).toBe(before.mailboxOf);
    expect(after.workersOf[parentKey]).toBe(before.workersOf[parentKey]);
  });

  it("selects the first listed requester on the requested node and retires removed routes", () => {
    const store = createFleetStore();
    const snapshot = read();
    const deployments = snapshot.client!.deployments;
    const first = session("a", "shared");
    const second = { ...first, requesterDid: "other", title: "other requester" };
    deployments[0]!.sessions = [first, second] as unknown as SessionSummary[];
    deployments[1]!.sessions = [session("b", "shared")] as unknown as SessionSummary[];
    applyFleetSnapshot(store, snapshot);
    expect(listedSession(store.getState(), "a", "shared")?.requesterDid).toBe("me");
    expect(listedSession(store.getState(), "b", "shared")?.nodeDid).toBe("b");
    expect(listedSession(store.getState(), "missing", "shared")).toBeNull();
    deployments[0]!.sessions = [second] as unknown as SessionSummary[];
    applyFleetSnapshot(store, snapshot);
    expect(listedSession(store.getState(), "a", "shared")?.requesterDid).toBe("other");
    deployments[0]!.sessions = [];
    applyFleetSnapshot(store, snapshot);
    expect(listedSession(store.getState(), "a", "shared")).toBeNull();
    expect(listedSession(store.getState(), "b", "shared")?.nodeDid).toBe("b");
  });

  it("looks up a route in a 10,000-session fleet without walking its session list", () => {
    const store = createFleetStore();
    const snapshot = read();
    snapshot.client!.deployments[0]!.sessions = Array.from({ length: 10_000 }, (_, i) =>
      session("a", `session-${i}`),
    ) as unknown as SessionSummary[];
    applyFleetSnapshot(store, snapshot);
    const state = store.getState();
    const expected = state.sessionsOf.a![9_999];
    let listReads = 0;
    const observed = {
      ...state,
      sessionsOf: {
        ...state.sessionsOf,
        a: new Proxy(state.sessionsOf.a!, {
          get(target, property, receiver) {
            listReads += 1;
            return Reflect.get(target, property, receiver);
          },
        }),
      },
    };
    for (let i = 0; i < 1_000; i += 1)
      expect(listedSession(observed, "a", "session-9999")).toBe(expected);
    expect(listedSession(observed, "a", "absent")).toBeNull();
    expect(listReads).toBe(0);
  });
});

it("does not display a different node when an explicitly selected node is unavailable", () => {
  const store = createFleetStore();
  applyFleetSnapshot(store, read());
  expect(nodeOrFirst(store.getState(), null)?.nodeDid).toBe("a");
  expect(nodeOrFirst(store.getState(), "not-loaded")).toBeNull();
});
