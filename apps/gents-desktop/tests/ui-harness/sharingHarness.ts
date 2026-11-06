import type {
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

/* Each is seen in the Sharing section of the node's configuration, in the
   shapes the runtime reports: requests to join the whole node, who has
   access, and what was decided. "sharing" holds one of everything;
   "sharing-empty" a node nobody shares yet; "sharing-unavailable" a runtime
   that can't share; "sharing-requests-only" a runtime that offers only
   requests to join, and "sharing-no-requests" one that offers all but
   those; "sharing-history-limited" a history cut at its limit, with
   decisions left out. */
export const SHARING_SCENARIOS = [
  "sharing",
  "sharing-empty",
  "sharing-unavailable",
  "sharing-requests-only",
  "sharing-no-requests",
  "sharing-history-limited",
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
};

const JACK: SharingParty = { did: "did:key:z6MkHarnessJack", name: "Jack" };
const BRUNO: SharingParty = { did: "did:key:z6MkHarnessBruno", name: "Bruno" };
const CHRIS: SharingParty = { did: "did:key:z6MkHarnessChris", name: "Chris" };
const PHONE: SharingParty = { did: "did:key:z6MkHarnessPhone", name: "David's phone" };
/* the runtime knows a party only by DID until a request carries a name */
const LAPTOP: SharingParty = { did: "did:key:z6MkHarnessLaptop", name: null };

const EVERYTHING: SharingAvailability = {
  requests: true,
  members: true,
  history: true,
};
const REQUESTS_ONLY: SharingAvailability = {
  requests: true,
  members: false,
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
/* the one profile the runtime admits, named as it names it */
const CLIENT: ShareProfile = { id: "client", name: "client", grants: [] };
/** the approved request behind Bruno's access */
export const HARNESS_BRUNO = "request-harness-bruno";

export function createHarnessSharing(
  scenario: SharingScenario,
  now: () => number = Date.now,
): SharingAdapter {
  if (scenario === "sharing-unavailable") return unavailableSharing;
  const at = (offset: number) => new Date(now() + offset).toISOString();
  const available = availability(scenario);
  const state: SharingState =
    scenario === "sharing-empty"
      ? { requests: [], members: [], history: [], historyLimits: null, available }
      : seeded(at, available, historyLimits(scenario));
  const listeners = new Set<() => void>();
  const changed = () => {
    for (const listener of listeners) listener();
  };
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

  return {
    /* a part the runtime doesn't offer reads as empty */
    async read() {
      const { available } = state;
      return structuredClone({
        ...state,
        requests: available.requests ? state.requests : [],
        members: available.members ? state.members : [],
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
  };
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
      scope: NODE,
      profile: CLIENT,
      requestedAt: at(-4 * MINUTE),
      expiresAt: at(56 * MINUTE),
    },
    {
      requestId: "request-harness-laptop",
      from: LAPTOP,
      scope: NODE,
      profile: CLIENT,
      requestedAt: at(-30 * 1000),
      expiresAt: at(4 * MINUTE + 30 * 1000),
    },
  ];
  const members: SharingMember[] = [
    {
      requestId: "request-harness-phone",
      party: PHONE,
      scope: NODE,
      profile: CLIENT,
      leaseExpiresAt: at(78 * DAY),
    },
    {
      requestId: HARNESS_BRUNO,
      party: BRUNO,
      scope: NODE,
      profile: CLIENT,
      leaseExpiresAt: at(2 * DAY),
    },
  ];
  const history: SharingDecision[] = [
    { kind: "approved", party: BRUNO, scope: NODE, by: HARNESS_ME, at: at(-12 * DAY) },
    { kind: "revoked", party: CHRIS, scope: NODE, by: HARNESS_ME, at: at(-3 * DAY) },
    { kind: "expired", party: JACK, scope: NODE, by: null, at: at(-2 * DAY) },
    {
      kind: "approved",
      party: PHONE,
      scope: NODE,
      by: HARNESS_ME,
      at: at(-12 * DAY - HOUR),
    },
  ];
  history.sort((a, b) => b.at.localeCompare(a.at));
  return { requests, members, history, historyLimits, available };
}
