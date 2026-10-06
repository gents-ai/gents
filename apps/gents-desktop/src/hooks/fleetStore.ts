import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DeploymentView,
  DesktopClientSnapshot,
  MailboxItemView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

/* Identity in the bridge's current words. The node and agent rename (gents
   #1799) changes these field names; the keys built here are what the rest
   of the store and its selectors use. */
export type NodeKey = string;
export type SessionKey = string;
export const nodeKeyOf = (node: { agentDid: string }): NodeKey => node.agentDid;
export const sessionKeyOf = (session: {
  agentDid: string;
  sessionId: string;
  requesterDid: string | null;
}): SessionKey =>
  `${session.agentDid}\u0000${session.sessionId}\u0000${session.requesterDid ?? ""}`;

/** A node as screens draw it: what a deployment says about itself, without
    the sessions and mailbox items it lists. */
export type NodeView = Omit<DeploymentView, "sessions" | "mailboxItems">;

export type FleetState = {
  /** the nodes in the snapshot's order */
  nodeKeys: readonly NodeKey[];
  nodes: Readonly<Record<NodeKey, NodeView>>;
  sessions: Readonly<Record<SessionKey, SessionSummary>>;
  /** each node's sessions, in the order it lists them */
  sessionsOf: Readonly<Record<NodeKey, readonly SessionSummary[]>>;
  /** each node's mailbox items, in the order it lists them */
  mailboxOf: Readonly<Record<NodeKey, readonly MailboxItemView[]>>;
  /** the session that handed each worker out, on whatever node lists it */
  parentOf: Readonly<Record<SessionKey, SessionSummary>>;
  /** the sessions each session handed out, on any node */
  workersOf: Readonly<Record<SessionKey, readonly SessionSummary[]>>;
  /** a session by its id alone, where only the id is known (a route) */
  bySessionId: Readonly<Record<string, SessionSummary>>;
};

export type FleetStore = StoreApi<FleetState>;

/** The node with this DID; null when the client cannot see it. */
export const nodeOf = (fleet: FleetState, agentDid: string | null | undefined) =>
  agentDid ? (fleet.nodes[nodeKeyOf({ agentDid })] ?? null) : null;

/** The first node in the snapshot's order; null before there is one. */
export const firstNode = (fleet: FleetState) => {
  const first = fleet.nodeKeys[0];
  return first ? (fleet.nodes[first] ?? null) : null;
};

/** A session as the node with this DID lists it. */
export function listedSession(
  fleet: FleetState,
  agentDid: string | null | undefined,
  sessionId: string | null | undefined,
): SessionSummary | null {
  if (!agentDid || !sessionId) return null;
  return (
    fleet.sessionsOf[nodeKeyOf({ agentDid })]?.find((s) => s.sessionId === sessionId) ??
    null
  );
}

const EMPTY: FleetState = {
  nodeKeys: [],
  nodes: {},
  sessions: {},
  sessionsOf: {},
  mailboxOf: {},
  parentOf: {},
  workersOf: {},
  bySessionId: {},
};

export function createFleetStore() {
  return createStore<FleetState>(() => EMPTY);
}

/**
 * Applies a read of the fleet. Until the bridge says which documents
 * changed, each read is the whole fleet; it is reconciled here, once, by
 * key, so an unchanged node, session or mailbox item keeps its identity
 * and a screen that reads it by key does not re-render. Derived indexes
 * are rebuilt only from entities that changed identity.
 */
export function applyFleetSnapshot(store: FleetStore, snapshot: DesktopClientSnapshot) {
  store.setState((prev) => reconcile(prev, snapshot));
}

function reconcile(prev: FleetState, snapshot: DesktopClientSnapshot): FleetState {
  const deployments = snapshot.client?.deployments ?? [];
  const nodes: Record<NodeKey, NodeView> = {};
  const sessions: Record<SessionKey, SessionSummary> = {};
  const sessionsOf: Record<NodeKey, readonly SessionSummary[]> = {};
  const mailboxOf: Record<NodeKey, readonly MailboxItemView[]> = {};
  for (const deployment of deployments) {
    const key = nodeKeyOf(deployment);
    const { sessions: listed, mailboxItems, ...node } = deployment;
    nodes[key] = keep(prev.nodes[key], node);
    const own = listed.map((session) => {
      const sessionKey = sessionKeyOf(session);
      const kept = keep(prev.sessions[sessionKey], session);
      /* a session listed by two nodes is the first node's */
      sessions[sessionKey] ??= kept;
      return kept;
    });
    sessionsOf[key] = keepList(prev.sessionsOf[key], own);
    const before = new Map((prev.mailboxOf[key] ?? []).map((m) => [m.itemId, m]));
    const items = mailboxItems.map((item) => keep(before.get(item.itemId), item));
    mailboxOf[key] = keepList(prev.mailboxOf[key], items);
  }
  const nodeKeys = deployments.map(nodeKeyOf);
  const sessionsChanged = !sameRecord(prev.sessions, sessions);
  const lineage = sessionsChanged
    ? lineageOf(sessions, prev)
    : {
        parentOf: prev.parentOf,
        workersOf: prev.workersOf,
        bySessionId: prev.bySessionId,
      };
  const next: FleetState = {
    nodeKeys: keepList(prev.nodeKeys, nodeKeys),
    nodes: sameRecord(prev.nodes, nodes) ? prev.nodes : nodes,
    sessions: sessionsChanged ? sessions : prev.sessions,
    sessionsOf: sameRecord(prev.sessionsOf, sessionsOf) ? prev.sessionsOf : sessionsOf,
    mailboxOf: sameRecord(prev.mailboxOf, mailboxOf) ? prev.mailboxOf : mailboxOf,
    ...lineage,
  };
  /* a read that changed nothing leaves the state, so no one is notified */
  return (Object.keys(next) as (keyof FleetState)[]).every(
    (key) => next[key] === prev[key],
  )
    ? prev
    : next;
}

/* The bridge names a worker's starting session exactly (node, session,
   requester), on whatever node lists it. */
function lineageOf(sessions: Record<SessionKey, SessionSummary>, prev: FleetState) {
  const parentOf: Record<SessionKey, SessionSummary> = {};
  const workers: Record<SessionKey, SessionSummary[]> = {};
  const bySessionId: Record<string, SessionSummary> = {};
  for (const [key, session] of Object.entries(sessions)) {
    bySessionId[session.sessionId] ??= session;
    const by = session.startedBy;
    const parent = by ? sessions[sessionKeyOf(by)] : undefined;
    if (!parent) continue;
    parentOf[key] = parent;
    (workers[sessionKeyOf(parent)] ??= []).push(session);
  }
  const workersOf: Record<SessionKey, readonly SessionSummary[]> = {};
  for (const [key, list] of Object.entries(workers))
    workersOf[key] = keepList(prev.workersOf[key], list);
  return {
    parentOf: sameRecord(prev.parentOf, parentOf) ? prev.parentOf : parentOf,
    workersOf: sameRecord(prev.workersOf, workersOf) ? prev.workersOf : workersOf,
    bySessionId: sameRecord(prev.bySessionId, bySessionId)
      ? prev.bySessionId
      : bySessionId,
  };
}

/* the previous object when the read says the same thing */
function keep<T>(prev: T | undefined, next: T): T {
  return prev !== undefined && equal(prev, next) ? prev : next;
}

/* the previous list when it holds the same objects in the same order */
function keepList<T>(prev: readonly T[] | undefined, next: readonly T[]): readonly T[] {
  return prev &&
    prev.length === next.length &&
    prev.every((item, i) => item === next[i])
    ? prev
    : next;
}

/* the same keys, each holding the same object */
function sameRecord<T>(a: Readonly<Record<string, T>>, b: Readonly<Record<string, T>>) {
  const keys = Object.keys(b);
  return (
    keys.length === Object.keys(a).length && keys.every((key) => a[key] === b[key])
  );
}

const isPlain = (value: object) => {
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
};

/** Equality of JSON values, as the bridge returns them. */
export function equal(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null)
    return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  /* anything but a plain object or array (a Set, a Date) is equal only to itself */
  if (!Array.isArray(a) && (!isPlain(a) || !isPlain(b))) return false;
  if (Array.isArray(a)) {
    const list = b as unknown[];
    return a.length === list.length && a.every((item, i) => equal(item, list[i]));
  }
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  return (
    keys.length === Object.keys(right).length &&
    keys.every((key) => key in right && equal(left[key], right[key]))
  );
}
