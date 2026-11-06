import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type JoinHistoryView,
  type JoinMemberView,
  type JoinRequestView,
} from "@source-inc/gents-desktop-client";

import {
  refuseSharing,
  unavailableSharing,
  type JoinRequest,
  type ShareProfile,
  type SharingAdapter,
  type SharingDecision,
  type SharingMember,
  type SharingParty,
} from "./sharing";

type JoinRequestApi = Required<
  Pick<
    DesktopApiAdapter,
    "listJoinRequests" | "decideJoinRequest" | "listJoinMembers" | "readJoinHistory"
  >
>;

/* the runtime announces no new requests, so an open page reads them again
   this often */
export const JOIN_REQUESTS_REFRESH_MS = 15_000;

/** Sharing as the runtime this desktop runs offers it: requests to join,
    approved with a lease or denied, who has access, revoked by their request,
    and what was decided. A runtime too old to list members and history offers
    requests alone. Another node's sharing isn't reachable from here, so it
    reads as unavailable and its actions are refused. */
export function createJoinRequestSharing({
  api,
  ownNodeDid,
  refreshMs = JOIN_REQUESTS_REFRESH_MS,
}: {
  api: JoinRequestApi;
  /** the node this desktop runs, once known */
  ownNodeDid: () => string | null;
  refreshMs?: number;
}): SharingAdapter {
  const listeners = new Map<string, Set<() => void>>();
  const changed = (nodeDid: string) => {
    for (const listener of listeners.get(nodeDid) ?? []) listener();
  };
  const runs = (nodeDid: string) => nodeDid === ownNodeDid();
  const decide = async (nodeDid: string, run: () => Promise<unknown>) => {
    if (!runs(nodeDid)) return refuseSharing();
    try {
      await run();
    } finally {
      /* a refused decision may mean the request is gone; read it again
         either way */
      changed(nodeDid);
    }
  };
  return {
    ...unavailableSharing,
    async read(nodeDid) {
      if (!runs(nodeDid)) return null;
      const [pending, members, history] = await Promise.all([
        answered(api.listJoinRequests(nodeDid)),
        answered(api.listJoinMembers(nodeDid)),
        answered(api.readJoinHistory(nodeDid)),
      ]);
      if (pending === null) return null;
      return {
        requests: pending.map(joinRequestOf),
        members: (members ?? []).map(memberOf),
        history: (history?.history ?? []).map(decisionOf),
        historyLimits: history && historyLimitsOf(history),
        available: {
          requests: true,
          members: members !== null,
          history: history !== null,
        },
      };
    },
    subscribe(nodeDid, onChange) {
      const own = listeners.get(nodeDid) ?? new Set<() => void>();
      listeners.set(nodeDid, own);
      own.add(onChange);
      /* also while the desktop's own node isn't known yet, so the page
         catches up once it is */
      const timer = setInterval(onChange, refreshMs);
      return () => {
        own.delete(onChange);
        clearInterval(timer);
      };
    },
    approve: (nodeDid, requestId, leaseSeconds) =>
      decide(nodeDid, () =>
        api.decideJoinRequest({
          nodeDid,
          requestId,
          decision: { kind: "approve", leaseSeconds },
        }),
      ),
    deny: (nodeDid, requestId) =>
      decide(nodeDid, () =>
        api.decideJoinRequest({ nodeDid, requestId, decision: { kind: "deny" } }),
      ),
    revoke: (nodeDid, requestId) =>
      decide(nodeDid, () =>
        api.decideJoinRequest({ nodeDid, requestId, decision: { kind: "revoke" } }),
      ),
  };
}

/* null when the bridge says this can't be answered: another node, or a
   runtime too old for the query */
async function answered<T>(read: Promise<T>): Promise<T | null> {
  try {
    return await read;
  } catch (error) {
    if (error instanceof BridgeInvokeError && error.code === "unsupported") return null;
    throw error;
  }
}

/* the runtime knows a party only by DID */
const partyOf = (did: string): SharingParty => ({ did, name: null });

/* and a profile only by name */
const profileOf = (name: string): ShareProfile => ({ id: name, name, grants: [] });

/** the runtime's pending request as the screens show it */
export function joinRequestOf(view: JoinRequestView): JoinRequest {
  return {
    requestId: view.requestId,
    from: partyOf(view.candidateDid),
    scope: { kind: "node" },
    profile: profileOf(view.profile ?? ""),
    requestedAt: view.issuedAt,
    expiresAt: view.expiresAt,
  };
}

/** the runtime's member as the screens show it: it gives the whole node, and
    says neither a name nor since when */
export function memberOf(view: JoinMemberView): SharingMember {
  return {
    requestId: view.requestId,
    party: partyOf(view.candidateDid),
    scope: { kind: "node" },
    profile: profileOf(view.profile),
    leaseExpiresAt: view.authorizationExpiresAt,
  };
}

/** one of the runtime's records as a line of history: a revoked record
    speaks of its revoking */
export function decisionOf(entry: JoinHistoryView["history"][number]): SharingDecision {
  return {
    kind: entry.state,
    party: partyOf(entry.candidateDid),
    scope: { kind: "node" },
    by: partyOf(entry.revokedBy ?? entry.decidedBy),
    at: entry.revokedAt ?? entry.decidedAt,
  };
}

const historyLimitsOf = (view: JoinHistoryView) => ({
  observed: view.observedCount,
  limit: view.limit,
  omitsSuperseded: view.omitsSuperseded,
  conflictsOmitted: view.conflictsOmitted,
});

/** the binding when the bridge offers join requests, and the stand-in
    otherwise */
export function sharingFor(
  api: DesktopApiAdapter,
  ownNodeDid: () => string | null,
): SharingAdapter {
  const {
    listJoinRequests: list,
    decideJoinRequest: decide,
    listJoinMembers: members,
    readJoinHistory: history,
  } = api;
  if (!list || !decide || !members || !history) return unavailableSharing;
  return createJoinRequestSharing({
    api: {
      listJoinRequests: (nodeDid) => list.call(api, nodeDid),
      decideJoinRequest: (request) => decide.call(api, request),
      listJoinMembers: (nodeDid) => members.call(api, nodeDid),
      readJoinHistory: (nodeDid) => history.call(api, nodeDid),
    },
    ownNodeDid,
  });
}
