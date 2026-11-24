import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopApiAdapter,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

import { createSelectors, type WithSelectors } from "./createSelectors";
import { heldFor } from "./sessionStore";
import type { ShellStores } from "./shellProjection";

export type ProvenanceState = { shown: SessionProvenanceView | null };

export type ProvenanceStore = WithSelectors<StoreApi<ProvenanceState>>;

export function createProvenanceStore() {
  return createSelectors(createStore<ProvenanceState>(() => ({ shown: null })));
}

export function uniquelyListed(
  sessions: readonly SessionSummary[] | undefined,
  nodeDid: string | null,
  sessionId: string | null,
): SessionSummary | null {
  const matches = (sessions ?? []).filter(
    (s) => s.nodeDid === nodeDid && s.sessionId === sessionId,
  );
  return matches.length === 1 ? matches[0]! : null;
}

type ProvenanceParams = {
  api: DesktopApiAdapter;
  stores: Pick<ShellStores, "selection" | "session" | "fleet"> & {
    provenance: ProvenanceStore;
  };
};

/**
 * The sessions of the open session's node whose turn or update can change its
 * lineage (Lean `LineageReread.lineageSessions`): the open session itself, every session its lineage names (its
 * starter, the sessions it started, messaged or heard from, the sessions its
 * calls caused, and the related sessions it reported), and any session that
 * names it as its starter (a child it has just started). A change anywhere
 * else on the node cannot move this lineage. What the open session's own rows
 * add moves `rowsRevision`, and a remote node's lineage moves
 * `provenanceVersion`.
 */
export function lineageSessions(
  sessions: readonly SessionSummary[] | undefined,
  nodeDid: string,
  sessionId: string,
  lineage: SessionProvenanceView | null,
): SessionSummary[] {
  const named = new Set<string>([sessionId]);
  const name = (link: { nodeDid: string; sessionId: string } | null | undefined) => {
    if (link && link.nodeDid === nodeDid) named.add(link.sessionId);
  };
  if (lineage) {
    name(lineage.startedBy);
    for (const link of [...lineage.started, ...lineage.sent, ...lineage.received])
      name(link);
    for (const sender of lineage.senders) name(sender.sender);
    for (const call of lineage.calls) name(call.caused);
    for (const related of lineage.relatedSessions ?? []) name(related);
  }
  return (sessions ?? []).filter(
    (s) =>
      named.has(s.sessionId) ||
      (s.startedBy?.nodeDid === nodeDid && s.startedBy.sessionId === sessionId),
  );
}

/** What the last lineage read was asked against: the scope and the open
 * session's rows, the remote lineage version, and each listed session's
 * moment. */
export type LineageAsked = {
  cues: string;
  version: number | null;
  listed: Map<string, string>;
};

/** A listed session's moment: its turn state and update time. */
export const sessionMoment = (s: SessionSummary) =>
  `${s.turnState ?? ""}:${s.updatedAt ?? ""}`;

/**
 * Whether the lineage is read again (Lean `LineageReread.asksAgain`): only
 * when the scope, rows or remote version changed, or a session that can move
 * the lineage (`movable`, from `lineageSessions`) has a moment the last read
 * did not see. The lineage a read brought naming more sessions is not itself
 * a change.
 */
export function asksAgain(
  asked: LineageAsked | null,
  cues: string,
  version: number | null,
  movable: readonly SessionSummary[],
): boolean {
  return (
    !asked ||
    asked.cues !== cues ||
    asked.version !== version ||
    movable.some((s) => asked.listed.get(s.sessionId) !== sessionMoment(s))
  );
}

export function createProvenance({ api, stores }: ProvenanceParams) {
  let held: { scope: string; value: SessionProvenanceView } | null = null;
  let asked: LineageAsked | null = null;
  let out = false;
  let missed = false;
  let watchers = 0;
  let stopWatching = () => {};

  function consider() {
    const { nodeDid, sessionId: selected } = stores.selection.getState();
    const state = stores.session.getState();
    const session = heldFor(state.session, selected, nodeDid);
    const sessionId = session?.sessionId ?? null;
    const sessions = nodeDid ? stores.fleet.getState().sessionsOf[nodeDid] : undefined;
    const summary = uniquelyListed(sessions, nodeDid, sessionId);
    const requesterDid = summary?.requesterDid ?? null;
    const scope = `${nodeDid ?? ""}\u0000${sessionId ?? ""}\u0000${requesterDid ?? ""}`;
    const shown = held?.scope === scope ? held.value : null;
    if (stores.provenance.getState().shown !== shown)
      stores.provenance.setState({ shown });

    if (!nodeDid || !sessionId || !summary) return;
    const version = session?.projectionRevision?.provenanceVersion ?? null;
    const cues = `${scope}\u0002${state.facts.rowsRevision}`;
    if (
      !asksAgain(
        asked,
        cues,
        version,
        lineageSessions(sessions, nodeDid, sessionId, shown),
      )
    )
      return;
    if (out) {
      missed = true;
      return;
    }
    asked = {
      cues,
      version,
      listed: new Map((sessions ?? []).map((s) => [s.sessionId, sessionMoment(s)])),
    };
    out = true;
    void api.sessionProvenance({ sessionId, nodeDid, requesterDid }).then(
      (value) => {
        held = { scope, value };
        landed();
      },
      () => {
        asked = null;
        if (missed) landed();
        else out = false;
      },
    );
  }

  function landed() {
    out = false;
    missed = false;
    if (watchers > 0) consider();
  }

  return {
    watchSessionProvenance() {
      if (watchers++ === 0) {
        const unsubscribe = [
          stores.selection.subscribe(consider),
          stores.session.subscribe(consider),
          stores.fleet.subscribe(consider),
        ];
        stopWatching = () => unsubscribe.forEach((stop) => stop());
        consider();
      }
      return () => {
        if (--watchers > 0) return;
        stopWatching();
        asked = null;
      };
    },
  };
}
