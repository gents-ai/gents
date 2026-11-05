import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type JoinRequestView,
} from "@source-inc/gents-desktop-client";

import {
  refuseSharing,
  unavailableSharing,
  type JoinRequest,
  type SharingAdapter,
  type SharingState,
} from "./sharing";

type JoinRequestApi = Required<
  Pick<DesktopApiAdapter, "listJoinRequests" | "decideJoinRequest">
>;

/* the runtime announces no new requests, so an open page reads them again
   this often */
export const JOIN_REQUESTS_REFRESH_MS = 15_000;

/** Sharing as the runtime this desktop runs offers it today: requests to
    join, approved with a lease or denied. Members, invites and history aren't
    offered yet, and another node's sharing isn't reachable from here, so
    those read as unavailable and their actions are refused. */
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
      let pending: JoinRequestView[];
      try {
        pending = await api.listJoinRequests(nodeDid);
      } catch (error) {
        if (error instanceof BridgeInvokeError && error.code === "unsupported")
          return null;
        throw error;
      }
      return joinRequestsOnly(pending.map(joinRequestOf));
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
  };
}

/** the runtime's pending request as the screens show it: the requester is
    known only by DID, and its profile only by name */
export function joinRequestOf(view: JoinRequestView): JoinRequest {
  const profile = view.profile ?? "";
  return {
    requestId: view.requestId,
    from: { did: view.candidateDid, name: null, device: false },
    scope: { kind: "node" },
    profile: { id: profile, name: profile, grants: [] },
    requestedAt: view.issuedAt,
    expiresAt: view.expiresAt,
    inviteId: null,
  };
}

function joinRequestsOnly(requests: JoinRequest[]): SharingState {
  return {
    requests,
    members: [],
    invites: [],
    history: [],
    available: { requests: true, members: false, invites: false, history: false },
  };
}

/** the binding when the bridge offers join requests, and the stand-in
    otherwise */
export function sharingFor(
  api: DesktopApiAdapter,
  ownNodeDid: () => string | null,
): SharingAdapter {
  const { listJoinRequests: list, decideJoinRequest: decide } = api;
  if (!list || !decide) return unavailableSharing;
  return createJoinRequestSharing({
    api: {
      listJoinRequests: (nodeDid) => list.call(api, nodeDid),
      decideJoinRequest: (request) => decide.call(api, request),
    },
    ownNodeDid,
  });
}
