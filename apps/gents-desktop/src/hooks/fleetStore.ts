import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DeploymentView,
  DesktopClientSnapshot,
  MailboxItemView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

export type NodeKey = string;
export type SessionKey = string;
export const nodeKeyOf = (node: { nodeDid: string }): NodeKey => node.nodeDid;
export const sessionKeyOf = (session: {
  nodeDid: string;
  sessionId: string;
  requesterDid: string | null;
}): SessionKey =>
  `${session.nodeDid}\u0000${session.sessionId}\u0000${session.requesterDid ?? ""}`;

export type NodeView = Omit<DeploymentView, "sessions" | "mailboxItems">;

export type FleetState = {
  nodeKeys: readonly NodeKey[];
  nodes: Readonly<Record<NodeKey, NodeView>>;
  sessions: Readonly<Record<SessionKey, SessionSummary>>;
  sessionsOf: Readonly<Record<NodeKey, readonly SessionSummary[]>>;
  byNodeSessionId: Readonly<Record<NodeKey, ReadonlyMap<string, SessionSummary>>>;
  mailboxOf: Readonly<Record<NodeKey, readonly MailboxItemView[]>>;
  parentOf: Readonly<Record<SessionKey, SessionSummary>>;
  workersOf: Readonly<Record<SessionKey, readonly SessionSummary[]>>;
  bySessionId: Readonly<Record<string, SessionSummary>>;
};

export type FleetStore = StoreApi<FleetState>;

export const nodeOf = (fleet: FleetState, nodeDid: string | null | undefined) =>
  nodeDid ? (fleet.nodes[nodeKeyOf({ nodeDid })] ?? null) : null;

export const firstNode = (fleet: FleetState) => {
  const first = fleet.nodeKeys[0];
  return first ? (fleet.nodes[first] ?? null) : null;
};

export const nodeOrFirst = (fleet: FleetState, nodeDid: string | null | undefined) =>
  nodeDid ? nodeOf(fleet, nodeDid) : firstNode(fleet);

export function listedSession(
  fleet: FleetState,
  nodeDid: string | null | undefined,
  sessionId: string | null | undefined,
): SessionSummary | null {
  if (!nodeDid || !sessionId) return null;
  return fleet.byNodeSessionId[nodeKeyOf({ nodeDid })]?.get(sessionId) ?? null;
}

const EMPTY: FleetState = {
  nodeKeys: [],
  nodes: {},
  sessions: {},
  sessionsOf: {},
  byNodeSessionId: {},
  mailboxOf: {},
  parentOf: {},
  workersOf: {},
  bySessionId: {},
};

export function createFleetStore() {
  return createStore<FleetState>(() => EMPTY);
}

export function applyFleetSnapshot(store: FleetStore, snapshot: DesktopClientSnapshot) {
  store.setState((prev) => reconcile(prev, snapshot));
}

function reconcile(prev: FleetState, snapshot: DesktopClientSnapshot): FleetState {
  const deployments = snapshot.client?.deployments ?? [];
  const nodes: Record<NodeKey, NodeView> = {};
  const sessions: Record<SessionKey, SessionSummary> = {};
  const sessionsOf: Record<NodeKey, readonly SessionSummary[]> = {};
  const byNodeSessionId: Record<NodeKey, ReadonlyMap<string, SessionSummary>> = {};
  const mailboxOf: Record<NodeKey, readonly MailboxItemView[]> = {};
  for (const deployment of deployments) {
    const key = nodeKeyOf(deployment);
    const { sessions: listed, mailboxItems, ...node } = deployment;
    nodes[key] = keep(prev.nodes[key], node);
    const own = listed.map((session) => {
      const sessionKey = sessionKeyOf(session);
      const kept = keep(prev.sessions[sessionKey], session);
      sessions[sessionKey] ??= kept;
      return kept;
    });
    sessionsOf[key] = keepList(prev.sessionsOf[key], own);
    if (sessionsOf[key] === prev.sessionsOf[key]) {
      byNodeSessionId[key] = prev.byNodeSessionId[key]!;
    } else {
      const byId = new Map<string, SessionSummary>();
      for (const session of own)
        if (!byId.has(session.sessionId)) byId.set(session.sessionId, session);
      byNodeSessionId[key] = byId;
    }
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
    byNodeSessionId: sameRecord(prev.byNodeSessionId, byNodeSessionId)
      ? prev.byNodeSessionId
      : byNodeSessionId,
    mailboxOf: sameRecord(prev.mailboxOf, mailboxOf) ? prev.mailboxOf : mailboxOf,
    ...lineage,
  };
  return (Object.keys(next) as (keyof FleetState)[]).every(
    (key) => next[key] === prev[key],
  )
    ? prev
    : next;
}

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

function keep<T>(prev: T | undefined, next: T): T {
  return prev !== undefined && equal(prev, next) ? prev : next;
}

function keepList<T>(prev: readonly T[] | undefined, next: readonly T[]): readonly T[] {
  return prev &&
    prev.length === next.length &&
    prev.every((item, i) => item === next[i])
    ? prev
    : next;
}

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

export function equal(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null)
    return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
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

export function shareUnchanged<T>(prev: unknown, next: T): T {
  if (Object.is(prev, next)) return next;
  if (
    typeof prev !== "object" ||
    typeof next !== "object" ||
    prev === null ||
    next === null
  )
    return next;
  if (Array.isArray(next)) {
    if (!Array.isArray(prev)) return next;
    let same = prev.length === next.length;
    const out = next.map((item: unknown, i) => {
      const shared = shareUnchanged(prev[i], item);
      if (shared !== prev[i]) same = false;
      return shared;
    });
    return (same ? prev : out) as T;
  }
  if (Array.isArray(prev) || !isPlain(prev) || !isPlain(next)) return next;
  const before = prev as Record<string, unknown>;
  const after = next as Record<string, unknown>;
  const keys = Object.keys(after);
  let same = keys.length === Object.keys(before).length;
  const out: Record<string, unknown> = {};
  for (const key of keys) {
    out[key] = shareUnchanged(before[key], after[key]);
    if (!(key in before) || out[key] !== before[key]) same = false;
  }
  return (same ? prev : out) as T;
}
