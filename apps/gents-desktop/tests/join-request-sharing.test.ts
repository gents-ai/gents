import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BridgeInvokeError,
  type JoinHistoryView,
  type JoinMemberView,
  type JoinRequestView,
} from "@source-inc/gents-desktop-client";

import {
  createJoinRequestSharing,
  decisionOf,
  joinRequestOf,
  memberOf,
  sharingFor,
} from "../src/hooks/joinRequestSharing";
import {
  DEFAULT_LEASE_SECONDS,
  MAX_LEASE_SECONDS,
  unavailableSharing,
} from "../src/hooks/sharing";
import { testApp } from "./app-fixture";
import { bootstrap } from "./config-panel-wiring/fixtures";

const OWN = "did:key:z6MkOwnNode";
const OTHER = "did:key:z6MkOtherNode";
const DAY = 24 * 60 * 60;

const pending = (overrides: Partial<JoinRequestView> = {}): JoinRequestView => ({
  requestId: "request-1",
  networkId: "network-1",
  candidateDid: "did:key:z6MkCandidateLaptop",
  candidatePeer: "peer-1",
  ownerAgent: "did:key:z6MkOwnerAgent",
  expiresAt: "2026-11-05T12:00:00Z",
  profile: "client",
  issuedAt: "2026-11-05T11:55:00Z",
  ...overrides,
});

const refusal = (code: "unsupported" | "notFound" | "unknown", message: string) =>
  new BridgeInvokeError({ code, message, retryable: false, endpoint: null });

const member = (overrides: Partial<JoinMemberView> = {}): JoinMemberView => ({
  requestId: "request-0",
  networkId: "network-1",
  candidateDid: "did:key:z6MkMemberPhone",
  candidatePeer: "peer-0",
  profile: "client",
  authorizationExpiresAt: "2026-12-05T12:00:00Z",
  ...overrides,
});

type HistoryEntry = JoinHistoryView["history"][number];

const entry = (overrides: Partial<HistoryEntry> = {}): HistoryEntry => ({
  requestId: "request-0",
  candidateDid: "did:key:z6MkMemberPhone",
  candidatePeer: "peer-0",
  profile: "client",
  state: "approved",
  decidedAt: "2026-11-01T12:00:00Z",
  decidedBy: "did:key:z6MkOperator",
  revokedAt: null,
  revokedBy: null,
  ...overrides,
});

const historyOf = (overrides: Partial<JoinHistoryView> = {}): JoinHistoryView => ({
  history: [entry()],
  observedCount: 1,
  limit: 50,
  omitsSuperseded: true,
  conflictsOmitted: false,
  ...overrides,
});

const TOO_OLD = "This node's runtime is too old to say who has access.";

function binding(list: JoinRequestView[] = [pending()]) {
  const api = {
    listJoinRequests: vi.fn(async (_nodeDid: string) => list),
    listJoinMembers: vi.fn(async (_nodeDid: string) => [member()]),
    readJoinHistory: vi.fn(async (_nodeDid: string) => historyOf()),
    decideJoinRequest: vi.fn(async () => ({
      requestId: "request-1",
      state: "approved",
      deliveryPending: false,
    })),
  };
  const sharing = createJoinRequestSharing({ api, ownNodeDid: () => OWN });
  return { api, sharing };
}

afterEach(() => vi.useRealTimers());

describe("the runtime's join requests as sharing", () => {
  it("maps a pending request to a node-wide request known by its DID", () => {
    expect(joinRequestOf(pending())).toEqual({
      requestId: "request-1",
      from: { did: "did:key:z6MkCandidateLaptop", name: null, device: false },
      scope: { kind: "node" },
      profile: { id: "client", name: "client", grants: [] },
      requestedAt: "2026-11-05T11:55:00Z",
      expiresAt: "2026-11-05T12:00:00Z",
      inviteId: null,
    });
  });

  it("keeps a request whose runtime names no profile or issue time", () => {
    const request = joinRequestOf(pending({ profile: null, issuedAt: null }));
    expect(request.requestedAt).toBeNull();
    expect(request.profile).toEqual({ id: "", name: "", grants: [] });
  });

  it("maps a member to a node-wide grant by DID, with no name and no start", () => {
    expect(memberOf(member())).toEqual({
      requestId: "request-0",
      party: { did: "did:key:z6MkMemberPhone", name: null, device: false },
      scope: { kind: "node" },
      profile: { id: "client", name: "client", grants: [] },
      leaseExpiresAt: "2026-12-05T12:00:00Z",
    });
  });

  it("maps a record to a line of history, a revoked one by its revoking", () => {
    expect(decisionOf(entry({ state: "denied" }))).toEqual({
      kind: "denied",
      party: { did: "did:key:z6MkMemberPhone", name: null, device: false },
      scope: { kind: "node" },
      by: { did: "did:key:z6MkOperator", name: null, device: false },
      at: "2026-11-01T12:00:00Z",
    });
    expect(
      decisionOf(
        entry({
          state: "revoked",
          revokedAt: "2026-11-04T12:00:00Z",
          revokedBy: "did:key:z6MkOtherOperator",
        }),
      ),
    ).toMatchObject({
      kind: "revoked",
      by: { did: "did:key:z6MkOtherOperator" },
      at: "2026-11-04T12:00:00Z",
    });
  });

  it("reads requests, members and history for its own node", async () => {
    const { api, sharing } = binding();
    const state = (await sharing.read(OWN))!;
    expect(api.listJoinRequests).toHaveBeenCalledWith(OWN);
    expect(api.listJoinMembers).toHaveBeenCalledWith(OWN);
    expect(api.readJoinHistory).toHaveBeenCalledWith(OWN);
    expect(state.requests.map((r) => r.requestId)).toEqual(["request-1"]);
    expect(state.members.map((m) => m.requestId)).toEqual(["request-0"]);
    expect(state.history.map((d) => d.kind)).toEqual(["approved"]);
    expect(state.historyLimits).toEqual({
      observed: 1,
      limit: 50,
      omitsSuperseded: true,
      conflictsOmitted: false,
    });
    expect(state.available).toEqual({
      requests: true,
      members: true,
      invites: false,
      history: true,
    });
  });

  it("offers requests alone from a runtime too old to list the rest", async () => {
    const { api, sharing } = binding();
    api.listJoinMembers.mockRejectedValueOnce(refusal("unsupported", TOO_OLD));
    api.readJoinHistory.mockRejectedValueOnce(refusal("unsupported", TOO_OLD));
    const state = (await sharing.read(OWN))!;
    expect(state.requests).toHaveLength(1);
    expect(state.members).toEqual([]);
    expect(state.history).toEqual([]);
    expect(state.historyLimits).toBeNull();
    expect(state.available).toEqual({
      requests: true,
      members: false,
      invites: false,
      history: false,
    });
  });

  it("passes on any other failure to list members or history", async () => {
    const { api, sharing } = binding();
    api.listJoinMembers.mockRejectedValueOnce(
      refusal("unknown", "runtime rejected enrollment query (400 Bad Request): nonce"),
    );
    await expect(sharing.read(OWN)).rejects.toThrow("nonce");
    api.readJoinHistory.mockRejectedValueOnce(
      refusal("unknown", "enrollment authority is not ready"),
    );
    await expect(sharing.read(OWN)).rejects.toThrow("not ready");
  });

  it("reads another node as unavailable without asking the bridge", async () => {
    const { api, sharing } = binding();
    expect(await sharing.read(OTHER)).toBeNull();
    expect(api.listJoinRequests).not.toHaveBeenCalled();
    expect(api.listJoinMembers).not.toHaveBeenCalled();
    expect(api.readJoinHistory).not.toHaveBeenCalled();
  });

  it("reads a node the bridge refuses as unavailable, and passes other failures on", async () => {
    const { api, sharing } = binding();
    api.listJoinRequests.mockRejectedValueOnce(
      refusal("unsupported", "not this desktop's"),
    );
    expect(await sharing.read(OWN)).toBeNull();
    api.listJoinRequests.mockRejectedValueOnce(
      refusal("unknown", "runtime is restarting"),
    );
    await expect(sharing.read(OWN)).rejects.toThrow("runtime is restarting");
  });

  it("approves with the chosen lease and denies, then reads again", async () => {
    const { api, sharing } = binding();
    const onChange = vi.fn();
    const stop = sharing.subscribe(OWN, onChange);
    await sharing.approve(OWN, "request-1", MAX_LEASE_SECONDS);
    expect(api.decideJoinRequest).toHaveBeenLastCalledWith({
      nodeDid: OWN,
      requestId: "request-1",
      decision: { kind: "approve", leaseSeconds: 90 * DAY },
    });
    await sharing.deny(OWN, "request-2");
    expect(api.decideJoinRequest).toHaveBeenLastCalledWith({
      nodeDid: OWN,
      requestId: "request-2",
      decision: { kind: "deny" },
    });
    expect(onChange).toHaveBeenCalledTimes(2);
    stop();
  });

  it("reads again after a refused decision, and passes the refusal on", async () => {
    const { api, sharing } = binding();
    const onChange = vi.fn();
    sharing.subscribe(OWN, onChange);
    api.decideJoinRequest.mockRejectedValueOnce(
      refusal("notFound", "That request was already decided or has expired."),
    );
    await expect(sharing.deny(OWN, "request-1")).rejects.toThrow("already decided");
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("revokes by the member's request, then reads again", async () => {
    const { api, sharing } = binding();
    const onChange = vi.fn();
    sharing.subscribe(OWN, onChange);
    await sharing.revoke(OWN, "request-0");
    expect(api.decideJoinRequest).toHaveBeenLastCalledWith({
      nodeDid: OWN,
      requestId: "request-0",
      decision: { kind: "revoke" },
    });
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("offers no renewing, since a lease can't be extended", () => {
    const { sharing } = binding();
    expect(sharing.renew).toBeUndefined();
  });

  it("refuses decisions for another node, and everything it doesn't offer", async () => {
    const { api, sharing } = binding();
    await expect(
      sharing.approve(OTHER, "request-1", DEFAULT_LEASE_SECONDS),
    ).rejects.toThrow("Sharing needs a newer runtime on this node.");
    await expect(sharing.deny(OTHER, "request-1")).rejects.toThrow("newer runtime");
    expect(api.decideJoinRequest).not.toHaveBeenCalled();
    await expect(sharing.revoke(OTHER, "request-1")).rejects.toThrow("newer runtime");
    await expect(sharing.profiles(OWN, { kind: "node" })).rejects.toThrow(
      "newer runtime",
    );
    await expect(sharing.readInvite("gents://invite/x")).rejects.toThrow(
      "newer runtime",
    );
  });

  it("reads an open page again while it is open, so new requests arrive", () => {
    vi.useFakeTimers();
    const api = {
      listJoinRequests: vi.fn(async () => []),
      listJoinMembers: vi.fn(async () => []),
      readJoinHistory: vi.fn(async () => historyOf()),
      decideJoinRequest: vi.fn(),
    };
    const sharing = createJoinRequestSharing({
      api,
      ownNodeDid: () => OWN,
      refreshMs: 1_000,
    });
    const onChange = vi.fn();
    const stop = sharing.subscribe(OWN, onChange);
    vi.advanceTimersByTime(2_500);
    expect(onChange).toHaveBeenCalledTimes(2);
    stop();
    vi.advanceTimersByTime(5_000);
    expect(onChange).toHaveBeenCalledTimes(2);
  });
});

describe("the app's sharing", () => {
  it("stays the stand-in while the bridge offers no join requests", () => {
    expect(sharingFor({} as never, () => OWN)).toBe(unavailableSharing);
  });

  it("binds the bridge's join requests to the node this desktop runs", async () => {
    const listJoinRequests = vi.fn(async () => [pending()]);
    const app = testApp({
      api: {
        listJoinRequests,
        listJoinMembers: vi.fn(async () => []),
        readJoinHistory: vi.fn(async () => historyOf()),
        decideJoinRequest: vi.fn(),
      },
      snapshot: {
        bootstrap: { ...bootstrap, initNodeDid: OWN },
        client: { deployments: [] },
      },
    });
    expect(app.sharing).not.toBe(unavailableSharing);
    expect((await app.sharing.read(OWN))?.requests).toHaveLength(1);
    expect(await app.sharing.read(OTHER)).toBeNull();
    expect(listJoinRequests).toHaveBeenCalledTimes(1);
  });
});
