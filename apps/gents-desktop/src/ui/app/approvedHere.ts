import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import {
  sameScope,
  type ShareProfile,
  type ShareScope,
  type SharingParty,
} from "../../hooks/sharing";
import { browserStorage } from "@/lib/storage";

/** An approval made from this desktop, kept because the node's runtime
    can't list who has access yet (#3470): without it, an approved request
    would simply vanish. It is a note on this device, not access itself. */
export type ApprovedHere = {
  requestId: string;
  party: SharingParty;
  scope: ShareScope;
  profile: ShareProfile;
  /** null for access that doesn't lapse */
  leaseExpiresAt: string | null;
  approvedAt: string;
};

type Store = { byNode: Record<string, ApprovedHere[]> };

const KEPT = 200;

const isText = (v: unknown): v is string => typeof v === "string";

function entryOf(v: unknown): ApprovedHere | null {
  if (typeof v !== "object" || v === null) return null;
  const e = v as Record<string, unknown>;
  const party = e.party as Record<string, unknown> | undefined;
  const scope = e.scope as Record<string, unknown> | undefined;
  const profile = e.profile as Record<string, unknown> | undefined;
  if (!isText(e.requestId) || !isText(e.approvedAt) || !party || !isText(party.did))
    return null;
  if (!scope || !["node", "pack", "agent"].includes(scope.kind as string)) return null;
  if (!profile || !isText(profile.name)) return null;
  if (e.leaseExpiresAt !== null && !isText(e.leaseExpiresAt)) return null;
  return v as ApprovedHere;
}

export function restoreApprovedHere(stored: unknown): Store {
  const byNode =
    typeof stored === "object" && stored !== null
      ? (stored as Record<string, unknown>).byNode
      : null;
  if (typeof byNode !== "object" || byNode === null) return { byNode: {} };
  const restored: Record<string, ApprovedHere[]> = {};
  for (const [node, entries] of Object.entries(byNode)) {
    if (!Array.isArray(entries)) continue;
    const kept = entries
      .map(entryOf)
      .filter((e): e is ApprovedHere => e !== null)
      .slice(0, KEPT);
    if (kept.length) restored[node] = kept;
  }
  return { byNode: restored };
}

const store = createStore<Store>()(
  persist(() => ({ byNode: {} }), {
    name: "gents-sharing-approved-here",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored) => restoreApprovedHere(stored),
  }),
);

const NONE: ApprovedHere[] = [];

export const useApprovedHere = (nodeDid: string) =>
  useStore(store, (s) => s.byNode[nodeDid] ?? NONE);

/** whether a member the runtime lists is the one this approval let in */
export const isApproval = (
  entry: ApprovedHere,
  member: { party: SharingParty; scope: ShareScope },
) => entry.party.did === member.party.did && sameScope(entry.scope, member.scope);

/** whether the approval's lease has run out by `now` */
export const lapsed = (entry: ApprovedHere, now: number) =>
  entry.leaseExpiresAt !== null && Date.parse(entry.leaseExpiresAt) <= now;

const edit = (nodeDid: string, change: (entries: ApprovedHere[]) => ApprovedHere[]) =>
  store.setState((s) => {
    const next = change(s.byNode[nodeDid] ?? NONE);
    const byNode = { ...s.byNode };
    if (next.length) byNode[nodeDid] = next;
    else delete byNode[nodeDid];
    return { byNode };
  });

export const approvedHere = {
  add(nodeDid: string, entry: ApprovedHere) {
    edit(nodeDid, (entries) =>
      [entry, ...entries.filter((e) => e.requestId !== entry.requestId)].slice(0, KEPT),
    );
  },
  forget(nodeDid: string, requestId: string) {
    edit(nodeDid, (entries) => entries.filter((e) => e.requestId !== requestId));
  },
  /** drops the approvals the runtime now lists, and those whose lease has
      passed */
  prune(
    nodeDid: string,
    members: readonly { party: SharingParty; scope: ShareScope }[],
    now: number,
  ) {
    const stale = (e: ApprovedHere) =>
      lapsed(e, now) || members.some((m) => isApproval(e, m));
    if ((store.getState().byNode[nodeDid] ?? NONE).some(stale))
      edit(nodeDid, (entries) => entries.filter((e) => !stale(e)));
  },
};

export const resetApprovedHere = () => store.setState({ byNode: {} }, true);
