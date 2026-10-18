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

export function createProvenance({ api, stores }: ProvenanceParams) {
  let held: { scope: string; value: SessionProvenanceView } | null = null;
  let asked: { cues: string; version: number | null } | null = null;
  let out = false;
  let missed = false;
  let watchers = 0;
  let stopWatching = () => {};
  let listed: { sessions: readonly SessionSummary[] | undefined; cue: string } = {
    sessions: undefined,
    cue: "",
  };

  function sessionsCue(sessions: readonly SessionSummary[] | undefined) {
    if (sessions !== listed.sessions)
      listed = {
        sessions,
        cue: (sessions ?? [])
          .map((s) => `${s.sessionId}:${s.turnState ?? ""}:${s.updatedAt ?? ""}`)
          .join(),
      };
    return listed.cue;
  }

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
    const cues = `${scope}\u0002${state.facts.rowsRevision}\u0002${sessionsCue(sessions)}`;
    if (asked?.cues === cues && asked.version === version) return;
    if (out) {
      missed = true;
      return;
    }
    asked = { cues, version };
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
