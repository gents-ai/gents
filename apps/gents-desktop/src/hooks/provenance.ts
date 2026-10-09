import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopApiAdapter,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

import { isLive } from "../lib/turnState";
import { createSelectors, type WithSelectors } from "./createSelectors";
import { heldFor } from "./sessionStore";
import type { ShellStores } from "./shellProjection";

/** The selected session's provenance as last read for its exact scope;
    null while none has been read for it. */
export type ProvenanceState = { shown: SessionProvenanceView | null };

export type ProvenanceStore = WithSelectors<StoreApi<ProvenanceState>>;

export function createProvenanceStore() {
  return createSelectors(createStore<ProvenanceState>(() => ({ shown: null })));
}

/* The listed session with this agent and label. Two listed scopes under one
   label are ambiguous, and neither is picked. */
export function uniquelyListed(
  sessions: readonly SessionSummary[] | undefined,
  agentDid: string | null,
  sessionId: string | null,
): SessionSummary | null {
  const matches = (sessions ?? []).filter(
    (s) => s.agentDid === agentDid && s.sessionId === sessionId,
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
 * The selected session's provenance, read while a screen shows it. Its
 * scope is exact: the session's agent, label and requester, as the session
 * list reports them. It is asked again when the transcript's rows change
 * (a streamed chunk to the live reply does not move them) or the agent's
 * session list does. While a request it caused is live it is also asked
 * whenever the session store's observation moves (`storeVersion`), so a
 * caused request settling anywhere this desktop observes refreshes it;
 * there is no timer of its own. Every applied live delta advances
 * `storeVersion`, so a settled lineage must not follow it: that would be
 * one bridge call per streamed chunk.
 *
 * One ask is out at a time: while the stream moves faster than a lineage
 * read, a newer ask would outdate every answer before it lands. A cue that
 * comes while one is out is asked once that one lands. A failed ask keeps
 * the last answer, and the next cue asks again.
 */
export function createProvenance({ api, stores }: ProvenanceParams) {
  let held: { scope: string; value: SessionProvenanceView } | null = null;
  /* what the last ask observed: a live lineage arriving starts following the
     store version without asking again for the one it was read at */
  let asked: { cues: string; version: number | null } | null = null;
  let out = false;
  let missed = false;
  let watchers = 0;
  let stopWatching = () => {};
  /* the agent's session list as a cue, rebuilt only when the list changes */
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
    const { agentDid, sessionId: selected } = stores.selection.getState();
    const state = stores.session.getState();
    const session = heldFor(state.session, selected, agentDid);
    const sessionId = session?.sessionId ?? null;
    const sessions = agentDid
      ? stores.fleet.getState().sessionsOf[agentDid]
      : undefined;
    const summary = uniquelyListed(sessions, agentDid, sessionId);
    const requesterDid = summary?.requesterDid ?? null;
    const scope = `${agentDid ?? ""}\u0000${sessionId ?? ""}\u0000${requesterDid ?? ""}`;
    const shown = held?.scope === scope ? held.value : null;
    if (stores.provenance.getState().shown !== shown)
      stores.provenance.setState({ shown });

    /* without the session's summary its exact scope is unknown */
    if (!agentDid || !sessionId || !summary) return;
    const version = session?.projectionRevision?.storeVersion ?? null;
    /* the store version is followed only while a request it caused is live */
    const followed = shown?.calls.some((c) => isLive(c.caused.lifecycleState))
      ? version
      : null;
    const cues = `${scope}\u0002${state.facts.rowsRevision}\u0002${sessionsCue(sessions)}`;
    if (asked?.cues === cues && (followed === null || asked.version === followed))
      return;
    if (out) {
      missed = true;
      return;
    }
    asked = { cues, version };
    out = true;
    void api.sessionProvenance({ sessionId, agentDid, requesterDid }).then(
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
    /** Reads the selected session's provenance while some screen shows it,
        and again on its cues, until every watcher has let go. */
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
        /* the next screen to show it asks afresh */
        asked = null;
      };
    },
  };
}
