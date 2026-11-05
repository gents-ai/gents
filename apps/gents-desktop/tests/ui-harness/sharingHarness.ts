import type { InstalledPack } from "@source-inc/gents-desktop-client";

import type {
  Invite,
  InviteDraft,
  InvitePreview,
  JoinRequest,
  ShareProfile,
  ShareScope,
  SharingAdapter,
  SharingAvailability,
  SharingDecision,
  SharingHistoryLimits,
  SharingMember,
  SharingParty,
  SharingState,
} from "../../src/hooks/sharing";
import { unavailableSharing } from "../../src/hooks/sharing";
import { joinSent, startJoin } from "../../src/ui/lib/joinInvite";

/* Each is seen in the Sharing section of the node's configuration.
   "sharing" holds one of everything; "sharing-empty" a node nobody shares
   yet; "sharing-unavailable" a runtime that can't share;
   "sharing-requests-only" a runtime that offers only requests to join, and
   "sharing-no-requests" one that offers all but those;
   "sharing-history-limited" a history cut at its limit, with decisions left
   out. The rest show the joining side: the join dialog open at an invite's
   preview to the whole node (from a device), to a pack this node lacks, or
   to one named agent; and, with the dialog closed, a request sent from an
   invite, waiting or approved. */
export const SHARING_SCENARIOS = [
  "sharing",
  "sharing-empty",
  "sharing-unavailable",
  "sharing-requests-only",
  "sharing-no-requests",
  "sharing-history-limited",
  "sharing-invite-node",
  "sharing-invite-pack",
  "sharing-invite-agent",
  "sharing-joining",
  "sharing-joined",
] as const;

export type SharingScenario = (typeof SHARING_SCENARIOS)[number];

export function isSharingScenario(
  value: string | null | undefined,
): value is SharingScenario {
  return (SHARING_SCENARIOS as readonly string[]).includes(value ?? "");
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

export const HARNESS_ME: SharingParty = {
  did: "did:key:z6MkHarnessDavid",
  name: "David",
  device: false,
};

const JACK: SharingParty = {
  did: "did:key:z6MkHarnessJack",
  name: "Jack",
  device: false,
};
const BRUNO: SharingParty = {
  did: "did:key:z6MkHarnessBruno",
  name: "Bruno",
  device: false,
};
const CHRIS: SharingParty = {
  did: "did:key:z6MkHarnessChris",
  name: "Chris",
  device: false,
};
const PHONE: SharingParty = {
  did: "did:key:z6MkHarnessPhone",
  name: "David's phone",
  device: true,
};
const LAPTOP: SharingParty = {
  did: "did:key:z6MkHarnessLaptop",
  name: null,
  device: true,
};

const EVERYTHING: SharingAvailability = {
  requests: true,
  members: true,
  invites: true,
  history: true,
};

const REQUESTS_ONLY: SharingAvailability = {
  requests: true,
  members: false,
  invites: false,
  history: false,
};

function availability(scenario: SharingScenario): SharingAvailability {
  if (scenario === "sharing-requests-only") return REQUESTS_ONLY;
  if (scenario === "sharing-no-requests") return { ...EVERYTHING, requests: false };
  return EVERYTHING;
}

const historyLimits = (scenario: SharingScenario): SharingHistoryLimits | null =>
  scenario === "sharing-history-limited"
    ? { observed: 73, limit: 50, omitsSuperseded: true, conflictsOmitted: true }
    : null;

const NODE: ShareScope = { kind: "node" };
export const HARNESS_GAME_PACK = "gents/tic_tac_toe";
const GAME: ShareScope = { kind: "pack", pack: HARNESS_GAME_PACK, name: "Tic-tac-toe" };
const COACH: ShareScope = { kind: "agent", agent: "coach", name: "Coach" };
const GAME_VERSION = "1.2.0";
/** the approved request behind Bruno's access to the game */
export const HARNESS_BRUNO_GAME = "request-harness-bruno-game";

/** the packs this node has installed in a scenario: the inviting side has
    the game, the side joining for it doesn't yet */
export function harnessInstalledPacks(scenario: SharingScenario): InstalledPack[] {
  if (scenario === "sharing-invite-pack") return [];
  return [
    {
      pack: HARNESS_GAME_PACK,
      installed: GAME_VERSION,
      latest: GAME_VERSION,
      outdated: false,
      error: null,
    },
  ];
}

const PROFILES = {
  device: {
    id: "device",
    name: "Your device",
    grants: ["Everything on this node, as you", "Sessions, agents, packs and settings"],
  },
  player: {
    id: "player",
    name: "Player",
    grants: ["See the board and the moves", "Make moves as themselves"],
  },
  viewer: {
    id: "viewer",
    name: "Viewer",
    grants: ["See the board and the moves"],
  },
  agentUser: {
    id: "agent-user",
    name: "Agent user",
    grants: ["Start sessions with this agent", "See the sessions they start"],
  },
} satisfies Record<string, ShareProfile>;

const profilesFor = (scope: ShareScope): ShareProfile[] =>
  scope.kind === "node"
    ? [PROFILES.device]
    : scope.kind === "pack"
      ? [PROFILES.player, PROFILES.viewer]
      : [PROFILES.agentUser];

/** invites' links and codes as the harness reads them back */
export const HARNESS_INVITE = {
  link: "gents://invite/harness-game",
  code: "GAME-7Q4K",
};
export const HARNESS_DEVICE_INVITE = {
  link: "gents://invite/harness-device",
  code: "DEVC-2M8P",
};
export const HARNESS_AGENT_INVITE = {
  link: "gents://invite/harness-coach",
  code: "COCH-5T3W",
};

export function createHarnessSharing(
  scenario: SharingScenario,
  now: () => number = Date.now,
): SharingAdapter {
  if (scenario === "sharing-unavailable") return unavailableSharing;
  const at = (offset: number) => new Date(now() + offset).toISOString();
  const available = availability(scenario);
  const state: SharingState =
    scenario === "sharing-empty"
      ? {
          requests: [],
          members: [],
          invites: [],
          history: [],
          historyLimits: null,
          available,
        }
      : seeded(at, available, historyLimits(scenario));
  const listeners = new Set<() => void>();
  const changed = () => {
    for (const listener of listeners) listener();
  };
  const sent = new Map<string, InvitePreview>();
  const send = (invite: { link: string; code: string }, preview: InvitePreview) => {
    sent.set(invite.link, preview);
    sent.set(invite.code, preview);
  };
  send(HARNESS_INVITE, gamePreview(at));
  send(HARNESS_DEVICE_INVITE, devicePreview(at));
  send(HARNESS_AGENT_INVITE, agentPreview(at));
  /* the requests sent from invites, still open to withdraw; the seeded
     scenarios' one among them */
  const asked = new Set<string>(["request-harness-seeded"]);
  let created = 0;
  let accepted = 0;

  const request = (requestId: string) => {
    const found = state.requests.find((r) => r.requestId === requestId);
    if (!found) throw new Error("That request was already decided or has expired.");
    return found;
  };
  const member = (requestId: string) => {
    const found = state.members.find((m) => m.requestId === requestId);
    if (!found) throw new Error("They no longer have access.");
    return found;
  };
  const decide = (decision: Omit<SharingDecision, "at" | "by">) =>
    state.history.unshift({ ...decision, by: HARNESS_ME, at: at(0) });
  const preview = (invite: string) => {
    const found = sent.get(invite.trim());
    if (!found) throw new Error("This invite isn't valid, or it has expired.");
    return found;
  };

  return {
    /* a part the runtime doesn't offer reads as empty */
    async read() {
      const { available } = state;
      return structuredClone({
        ...state,
        requests: available.requests ? state.requests : [],
        members: available.members ? state.members : [],
        invites: available.invites ? state.invites : [],
        history: available.history ? state.history : [],
      });
    },
    subscribe(_nodeDid, onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    async approve(_nodeDid, requestId, leaseSeconds) {
      const r = request(requestId);
      state.requests = state.requests.filter((x) => x !== r);
      state.members.push({
        requestId,
        party: r.from,
        scope: r.scope,
        profile: r.profile,
        leaseExpiresAt: at(leaseSeconds * 1000),
      });
      decide({ kind: "approved", party: r.from, scope: r.scope });
      changed();
    },
    async deny(_nodeDid, requestId) {
      const r = request(requestId);
      state.requests = state.requests.filter((x) => x !== r);
      decide({ kind: "denied", party: r.from, scope: r.scope });
      changed();
    },
    async revoke(_nodeDid, requestId) {
      const m = member(requestId);
      state.members = state.members.filter((x) => x !== m);
      decide({ kind: "revoked", party: m.party, scope: m.scope });
      changed();
    },
    async renew(_nodeDid, requestId, leaseSeconds) {
      member(requestId).leaseExpiresAt = at(leaseSeconds * 1000);
      changed();
    },
    async profiles(_nodeDid, scope) {
      return profilesFor(scope);
    },
    async createInvite(_nodeDid, draft: InviteDraft) {
      const profile = profilesFor(draft.scope).find((p) => p.id === draft.profileId);
      if (!profile) throw new Error("That profile isn't offered for this share.");
      created += 1;
      const inviteId = `invite-harness-${created}`;
      const invite: Invite = {
        inviteId,
        kind: draft.kind,
        scope: draft.scope,
        profile,
        createdAt: at(0),
        expiresAt: at(draft.expiresInSeconds * 1000),
        usesLeft: draft.uses,
        preApproved: draft.preApproved,
        link: `gents://invite/${inviteId}`,
        code: `HRN-${String(created).padStart(4, "0")}`,
        qr: `gents://invite/${inviteId}`,
      };
      state.invites.unshift(invite);
      send(invite, {
        from: HARNESS_ME,
        kind: invite.kind,
        scope: invite.scope,
        profile,
        installs:
          invite.scope.kind === "pack"
            ? [{ pack: invite.scope.pack, version: GAME_VERSION }]
            : [],
        expiresAt: invite.expiresAt,
        preApproved: invite.preApproved,
      });
      changed();
      return structuredClone(invite);
    },
    async cancelInvite(_nodeDid, inviteId) {
      const invite = state.invites.find((i) => i.inviteId === inviteId);
      state.invites = state.invites.filter((i) => i.inviteId !== inviteId);
      if (invite) {
        sent.delete(invite.link);
        sent.delete(invite.code);
      }
      changed();
    },
    async readInvite(invite) {
      return structuredClone(preview(invite));
    },
    async acceptInvite(invite) {
      const found = preview(invite);
      accepted += 1;
      const requestId = `request-harness-accepted-${accepted}`;
      if (!found.preApproved) asked.add(requestId);
      return {
        requestId,
        state: found.preApproved ? "approved" : "pending_approval",
      };
    },
    async cancelRequest(requestId) {
      if (!asked.delete(requestId))
        throw new Error("That request was already decided or withdrawn.");
    },
  };
}

/** Opens the joining side's state a scenario names: the join dialog at an
    invite's preview, as a link opened from outside would, or a request
    already sent. */
export function seedHarnessJoining(scenario: string | null, now = Date.now()) {
  const at = (offset: number) => new Date(now + offset).toISOString();
  if (scenario === "sharing-invite-node") startJoin(HARNESS_DEVICE_INVITE.link);
  else if (scenario === "sharing-invite-pack") startJoin(HARNESS_INVITE.link);
  else if (scenario === "sharing-invite-agent") startJoin(HARNESS_AGENT_INVITE.code);
  else if (scenario === "sharing-joining" || scenario === "sharing-joined")
    joinSent({
      invite: HARNESS_INVITE.link,
      preview: gamePreview(at),
      acceptance: {
        requestId: "request-harness-seeded",
        state: scenario === "sharing-joined" ? "approved" : "pending_approval",
      },
    });
}

function seeded(
  at: (offset: number) => string,
  available: SharingAvailability,
  historyLimits: SharingHistoryLimits | null,
): SharingState {
  const requests: JoinRequest[] = [
    {
      requestId: "request-harness-jack",
      from: JACK,
      scope: GAME,
      profile: PROFILES.player,
      requestedAt: at(-4 * MINUTE),
      expiresAt: at(56 * MINUTE),
      inviteId: "invite-harness-game",
    },
    {
      requestId: "request-harness-laptop",
      from: LAPTOP,
      scope: NODE,
      profile: PROFILES.device,
      requestedAt: at(-30 * 1000),
      expiresAt: at(4 * MINUTE + 30 * 1000),
      inviteId: null,
    },
  ];
  const members: SharingMember[] = [
    {
      requestId: "request-harness-phone",
      party: PHONE,
      scope: NODE,
      profile: PROFILES.device,
      leaseExpiresAt: null,
    },
    {
      requestId: HARNESS_BRUNO_GAME,
      party: BRUNO,
      scope: GAME,
      profile: PROFILES.player,
      leaseExpiresAt: at(2 * DAY),
    },
    {
      requestId: "request-harness-bruno-coach",
      party: BRUNO,
      scope: COACH,
      profile: PROFILES.agentUser,
      leaseExpiresAt: at(78 * DAY),
    },
  ];
  const invites: Invite[] = [
    {
      inviteId: "invite-harness-game",
      kind: "person",
      scope: GAME,
      profile: PROFILES.player,
      createdAt: at(-1 * HOUR),
      expiresAt: at(3 * DAY),
      usesLeft: 2,
      preApproved: false,
      link: HARNESS_INVITE.link,
      code: HARNESS_INVITE.code,
      qr: HARNESS_INVITE.link,
    },
    {
      inviteId: "invite-harness-coach",
      kind: "person",
      scope: COACH,
      profile: PROFILES.agentUser,
      createdAt: at(-1 * HOUR),
      expiresAt: at(5 * HOUR),
      usesLeft: null,
      preApproved: true,
      link: HARNESS_AGENT_INVITE.link,
      code: HARNESS_AGENT_INVITE.code,
      qr: HARNESS_AGENT_INVITE.link,
    },
  ];
  const history: SharingDecision[] = [
    { kind: "approved", party: BRUNO, scope: GAME, by: HARNESS_ME, at: at(-12 * DAY) },
    {
      kind: "approved",
      party: BRUNO,
      scope: COACH,
      by: HARNESS_ME,
      at: at(-12 * DAY + MINUTE),
    },
    { kind: "revoked", party: CHRIS, scope: GAME, by: HARNESS_ME, at: at(-3 * DAY) },
    { kind: "expired", party: JACK, scope: GAME, by: null, at: at(-2 * DAY) },
    { kind: "approved", party: PHONE, scope: NODE, by: HARNESS_ME, at: at(-40 * DAY) },
  ];
  history.sort((a, b) => b.at.localeCompare(a.at));
  return { requests, members, invites, history, historyLimits, available };
}

function gamePreview(at: (offset: number) => string): InvitePreview {
  return {
    from: HARNESS_ME,
    kind: "person",
    scope: GAME,
    profile: PROFILES.player,
    installs: [{ pack: HARNESS_GAME_PACK, version: GAME_VERSION }],
    expiresAt: at(3 * DAY),
    preApproved: false,
  };
}

function devicePreview(at: (offset: number) => string): InvitePreview {
  return {
    from: HARNESS_ME,
    kind: "device",
    scope: NODE,
    profile: PROFILES.device,
    installs: [],
    expiresAt: at(50 * MINUTE),
    preApproved: true,
  };
}

/* an inviter whose node gives no name, so the preview has only its DID */
function agentPreview(at: (offset: number) => string): InvitePreview {
  return {
    from: { ...HARNESS_ME, name: null },
    kind: "person",
    scope: COACH,
    profile: PROFILES.agentUser,
    installs: [],
    expiresAt: at(5 * HOUR),
    preApproved: true,
  };
}
