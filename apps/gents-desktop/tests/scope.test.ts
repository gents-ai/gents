/* Scope: which nodes and agents a list covers, and the URL it can be
   linked by. The default is the shell's selected node, which is what
   every screen shows today. */
import type {
  DeploymentView,
  MailboxItemView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  defaultScope,
  knownNodeIds,
  mailboxInScope,
  nodesInScope,
  recentInScope,
  sessionsInScope,
} from "@/lib/scope";
import { workingNode } from "@/lib/nodes";
import { fleetFor } from "./shell-fixture";

const session = (id: string, nodeDid: string, agentId: string, updatedAt: string) =>
  ({ sessionId: id, nodeDid, agentId, updatedAt }) as unknown as SessionSummary;
const item = (id: string, nodeDid: string, targetAgentId: string, status = "open") =>
  ({ itemId: id, nodeDid, targetAgentId, status }) as unknown as MailboxItemView;
const node = (
  nodeDid: string,
  source: string,
  sessions: SessionSummary[],
  mailboxItems: MailboxItemView[] = [],
) => ({ nodeDid, source, sessions, mailboxItems }) as unknown as DeploymentView;

const local = node(
  "did:local",
  "local",
  [
    session("s1", "did:local", "did:local:eng", "2026-09-26T10:00:00Z"),
    session("s2", "did:local", "did:local:review", "2026-09-26T12:00:00Z"),
  ],
  [
    item("m1", "did:local", "did:local:eng"),
    item("m2", "did:local", "did:local:eng", "dismissed"),
  ],
);
const peer = node(
  "did:peer",
  "peer",
  [session("p1", "did:peer", "did:peer:eng", "2026-09-26T11:00:00Z")],
  [item("m3", "did:peer", "did:peer:eng")],
);
const ctx = {
  nodes: [local, peer],
  selectedNodeDid: "did:local",
  homeDid: null,
  fleet: fleetFor([local, peer]).getState(),
};

describe("nodes in scope", () => {
  it("sessions default to the working node whatever is selected; the mailbox to every node", () => {
    expect(nodesInScope(defaultScope("sessions"), ctx)).toEqual([local]);
    expect(
      nodesInScope(defaultScope("sessions"), { ...ctx, selectedNodeDid: "did:peer" }),
    ).toEqual([local]);
    expect(nodesInScope(defaultScope("mailbox"), ctx)).toEqual([local, peer]);
    expect(
      nodesInScope(
        { nodes: "selected", agents: [] },
        { ...ctx, selectedNodeDid: "did:peer" },
      ),
    ).toEqual([peer]);
  });
  it("working is the node this machine runs, or the selection when it runs none", () => {
    expect(nodesInScope({ nodes: "working", agents: [] }, ctx)).toEqual([local]);
    expect(
      nodesInScope(
        { nodes: "working", agents: [] },
        { nodes: [peer], selectedNodeDid: "did:peer", homeDid: null },
      ),
    ).toEqual([peer]);
    expect(workingNode([peer], null)).toBeNull();
  });
  it("an enrolled node is the working node when it is the home the caller names", () => {
    const home = node("did:home", "enrollment", []);
    expect(workingNode([peer, home], null)).toBeNull();
    expect(workingNode([peer, home], "did:home")).toBe(home);
    expect(
      nodesInScope(
        { nodes: "working", agents: [] },
        {
          nodes: [peer, home],
          selectedNodeDid: "did:peer",
          homeDid: "did:home",
        },
      ),
    ).toEqual([home]);
  });
  it("all and a chosen set span nodes in the snapshot order", () => {
    expect(nodesInScope({ nodes: "all", agents: [] }, ctx)).toEqual([local, peer]);
    expect(nodesInScope({ nodes: ["did:peer"], agents: [] }, ctx)).toEqual([peer]);
  });
});

describe("lists in scope", () => {
  it("merges sessions across nodes and narrows by agent", () => {
    expect(
      sessionsInScope({ nodes: "all", agents: [] }, ctx).map((s) => s.sessionId),
    ).toEqual(["s1", "s2", "p1"]);
    expect(
      sessionsInScope({ nodes: "all", agents: ["did:local:eng"] }, ctx).map(
        (s) => s.sessionId,
      ),
    ).toEqual(["s1"]);
  });
  it("lists open mail from every node in scope", () => {
    expect(
      mailboxInScope({ nodes: "all", agents: [] }, ctx).map((m) => m.itemId),
    ).toEqual(["m1", "m3"]);
    expect(mailboxInScope(defaultScope("mailbox"), ctx).map((m) => m.itemId)).toEqual([
      "m1",
      "m3",
    ]);
  });
  it("recents are newest first across the scope", () => {
    expect(
      recentInScope({ nodes: "all", agents: [] }, ctx, 2).map((s) => s.sessionId),
    ).toEqual(["s2", "p1"]);
  });
});

describe("stored node picks", () => {
  it("drop DIDs no node has once nodes arrive, and stand before then", () => {
    expect(knownNodeIds(["did:gone", "did:peer"], ctx)).toEqual(["did:peer"]);
    expect(knownNodeIds(["did:gone"], ctx)).toEqual([]);
    expect(knownNodeIds(["did:gone"], { ...ctx, nodes: [] })).toEqual(["did:gone"]);
  });
});
