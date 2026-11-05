/**
 * Who may use a node, and the invites that let them in.
 *
 * Every sharing screen reads and acts through a {@link SharingAdapter}. Invites,
 * joining, and any scope narrower than the whole node are the desktop's stand-in
 * until the runtime offers them (#3472, #3495): replace those shapes with the
 * runtime's generated types when they exist, and never write them to a node.
 */

/** what a share covers: the whole node, one pack's data and pages, or one
    named agent. An agent is granted on its own, never as part of a pack's
    share; the agent scope is the desktop's stand-in until the runtime grants
    named agents (#3495). */
export type ShareScope =
  | { kind: "node" }
  | { kind: "pack"; pack: string; name: string }
  | { kind: "agent"; agent: string; name: string };

/** what the other side may do, in words the screen shows as they are */
export type ShareProfile = {
  id: string;
  name: string;
  grants: string[];
};

/** a node on the other side; `device` is one of the person's own devices */
export type SharingParty = {
  did: string;
  name: string | null;
  device: boolean;
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
  /** the invite it came from, if any */
  inviteId: string | null;
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

export type InviteKind = "device" | "person";

/** what the person chooses when making an invite */
export type InviteDraft = {
  kind: InviteKind;
  scope: ShareScope;
  profileId: string;
  expiresInSeconds: number;
  /** null for any number of uses */
  uses: number | null;
  /** joining needs no second approval */
  preApproved: boolean;
};

export type Invite = {
  inviteId: string;
  kind: InviteKind;
  scope: ShareScope;
  profile: ShareProfile;
  createdAt: string;
  expiresAt: string;
  usesLeft: number | null;
  preApproved: boolean;
  link: string;
  code: string;
  /** the text a QR code encodes */
  qr: string;
};

/** whether two scopes name the same share */
export const sameScope = (a: ShareScope, b: ShareScope) =>
  a.kind === "node"
    ? b.kind === "node"
    : a.kind === "pack"
      ? b.kind === "pack" && a.pack === b.pack
      : b.kind === "agent" && a.agent === b.agent;

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
  invites: boolean;
  history: boolean;
};

export type SharingState = {
  requests: JoinRequest[];
  members: SharingMember[];
  invites: Invite[];
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

/** an invite as the person it was sent to sees it, before joining */
export type InvitePreview = {
  from: SharingParty;
  kind: InviteKind;
  scope: ShareScope;
  profile: ShareProfile;
  /** packs joining installs, by name and version, and the digest that pins
      the archive when the invite names one */
  installs: { pack: string; version: string; digest?: string }[];
  expiresAt: string;
  preApproved: boolean;
};

/** where joining stands after accepting an invite */
export type InviteAcceptance = {
  requestId: string;
  state: "pending_approval" | "approved";
};

export interface SharingAdapter {
  /** the node's sharing as it stands, or null when its runtime can't share */
  read(nodeDid: string): Promise<SharingState | null>;
  /** calls `onChange` whenever the node's sharing may have changed */
  subscribe(nodeDid: string, onChange: () => void): () => void;
  approve(nodeDid: string, requestId: string, leaseSeconds: number): Promise<void>;
  deny(nodeDid: string, requestId: string): Promise<void>;
  /** ends the access a member's approved request gave */
  revoke(nodeDid: string, requestId: string): Promise<void>;
  /** extends a member's lease; absent where the runtime can't. A signed
      approval carries one fixed expiry, so today's runtime renews only by the
      member enrolling again. */
  renew?(nodeDid: string, requestId: string, leaseSeconds: number): Promise<void>;
  /** the profiles an invite may carry for this scope */
  profiles(nodeDid: string, scope: ShareScope): Promise<ShareProfile[]>;
  createInvite(nodeDid: string, draft: InviteDraft): Promise<Invite>;
  cancelInvite(nodeDid: string, inviteId: string): Promise<void>;
  /** reads a link or code without joining; rejects when it isn't valid */
  readInvite(invite: string): Promise<InvitePreview>;
  acceptInvite(invite: string): Promise<InviteAcceptance>;
  /** withdraws a request this desktop sent */
  cancelRequest(requestId: string): Promise<void>;
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
  profiles: refuseSharing,
  createInvite: refuseSharing,
  cancelInvite: refuseSharing,
  readInvite: refuseSharing,
  acceptInvite: refuseSharing,
  cancelRequest: refuseSharing,
};

/** refuses a sharing action the node's runtime doesn't offer */
export async function refuseSharing(): Promise<never> {
  throw new Error(UNAVAILABLE);
}
