/**
 * Who may use a node: the requests to join it, who has access, and what was
 * decided. Every sharing screen reads and acts through a
 * {@link SharingAdapter}.
 */

/** what a share covers. The runtime grants only the whole node; a narrower
    scope joins this union when it grants one (#3471, #3495). */
export type ShareScope = { kind: "node" };

/** what the other side may do, in words the screen shows as they are */
export type ShareProfile = {
  id: string;
  name: string;
  grants: string[];
};

/** a node on the other side, by its DID, and the name it goes by here */
export type SharingParty = {
  did: string;
  name: string | null;
};

/** someone asking to join, waiting for a decision */
export type JoinRequest = {
  requestId: string;
  from: SharingParty;
  scope: ShareScope;
  profile: ShareProfile;
  /** null when the node's runtime doesn't say */
  requestedAt: string | null;
  expiresAt: string;
};

/** someone with access now, one grant per scope: a person given a pack and
    an agent is two members, revoked apart */
export type SharingMember = {
  /** the approved request that gave this access, which revoking names */
  requestId: string;
  party: SharingParty;
  scope: ShareScope;
  profile: ShareProfile;
  /** null for access that doesn't lapse */
  leaseExpiresAt: string | null;
};

export type SharingDecisionKind =
  "approved" | "denied" | "revoked" | "expired" | "left";

/** one line of a node's sharing history */
export type SharingDecision = {
  kind: SharingDecisionKind;
  party: SharingParty;
  scope: ShareScope;
  /** who decided; null when nobody did, as when a request expires */
  by: SharingParty | null;
  at: string;
};

/** what a node's history leaves out, as its runtime reports it */
export type SharingHistoryLimits = {
  /** how many records the runtime holds, before its limit */
  observed: number;
  limit: number;
  /** a record replaced by a later one isn't listed */
  omitsSuperseded: boolean;
  /** a record in conflict isn't listed */
  conflictsOmitted: boolean;
};

/** which parts of a node's sharing its runtime offers; a part it doesn't
    offer reads as empty, which is not the same as none */
export type SharingAvailability = {
  requests: boolean;
  members: boolean;
  history: boolean;
};

export type SharingState = {
  requests: JoinRequest[];
  members: SharingMember[];
  history: SharingDecision[];
  /** null when the runtime reports no limits */
  historyLimits: SharingHistoryLimits | null;
  available: SharingAvailability;
};

const DAY_SECONDS = 24 * 60 * 60;

/** how long an approval lasts unless the person chooses otherwise, and the
    longest the runtime accepts: the protocol's
    DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS and
    MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS */
export const DEFAULT_LEASE_SECONDS = 30 * DAY_SECONDS;
export const MAX_LEASE_SECONDS = 90 * DAY_SECONDS;

export interface SharingAdapter {
  /** the node's sharing as it stands, or null when its runtime can't share */
  read(nodeDid: string): Promise<SharingState | null>;
  /** calls `onChange` whenever the node's sharing may have changed */
  subscribe(nodeDid: string, onChange: () => void): () => void;
  approve(nodeDid: string, requestId: string, leaseSeconds: number): Promise<void>;
  deny(nodeDid: string, requestId: string): Promise<void>;
  /** ends the access a member's approved request gave */
  revoke(nodeDid: string, requestId: string): Promise<void>;
}

const UNAVAILABLE = "Sharing needs a newer runtime on this node.";

/** the adapter until the runtime offers sharing: nothing to read, and every
    action refused with words the screen can show */
export const unavailableSharing: SharingAdapter = {
  read: async () => null,
  subscribe: () => () => {},
  approve: refuseSharing,
  deny: refuseSharing,
  revoke: refuseSharing,
};

/** refuses a sharing action the node's runtime doesn't offer */
export async function refuseSharing(): Promise<never> {
  throw new Error(UNAVAILABLE);
}
