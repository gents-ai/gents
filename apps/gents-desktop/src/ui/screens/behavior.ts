import type { NodeView } from "../../hooks/fleetStore";
import { agentOf } from "@/lib/agents";
import { seedHash } from "@/lib/avatar";

const ARTICLES = new Set(["the", "a", "an"]);

export function initials(name: string) {
  const all = name.trim().split(/\s+/);
  const words =
    all.length > 1 && ARTICLES.has(all[0]!.toLowerCase()) ? all.slice(1) : all;
  return (
    words.length > 1 ? words[0]![0]! + words[1]![0]! : words.join(" ").slice(0, 2)
  ).replace(/^(.)(.)$/, (_, a: string, b: string) => a.toUpperCase() + b.toLowerCase());
}

export const UNKNOWN_AGENT = "Unknown agent";

/** The end of an agent's DID, enough to tell apart agents that share a name. */
export const didTail = (agentDid: string) => `…${agentDid.slice(-6)}`;

/** An agent's name as `deployment` lists it. One it does not list (this
    person can't read it, it runs elsewhere, or it was deleted) is unknown,
    never the node's default, and keeps its DID's tail. */
export function agentName(agentDid: string | null, deployment: NodeView | null) {
  const agent = agentOf(deployment, agentDid);
  if (agent) return agent.displayName;
  return agentDid ? `${UNKNOWN_AGENT} ${didTail(agentDid)}` : UNKNOWN_AGENT;
}

export const isUnknownAgent = (name: string) =>
  name === UNKNOWN_AGENT || name.startsWith(`${UNKNOWN_AGENT} …`);

const nameKey = (name: string) => name.trim().toLowerCase();

/** The names, compared loosely, that more than one of `agents` goes by. */
export function sharedNames(
  agents: Iterable<{ agentDid: string | null; displayName: string }>,
): Set<string> {
  const first = new Map<string, string | null>();
  const shared = new Set<string>();
  for (const { agentDid, displayName } of agents) {
    const key = nameKey(displayName);
    if (!first.has(key)) first.set(key, agentDid);
    else if (first.get(key) !== agentDid) shared.add(key);
  }
  return shared;
}

export const isShared = (shared: ReadonlySet<string>, displayName: string) =>
  shared.has(nameKey(displayName));

/** Whether another agent on `deployment` goes by `agentDid`'s name. */
export function nameShared(agentDid: string | null, deployment: NodeView | null) {
  const agent = agentOf(deployment, agentDid);
  return Boolean(
    agent && deployment && isShared(sharedNames(deployment.agents), agent.displayName),
  );
}

/** The hue an agent's initials are tinted with when its name is shared:
    from its DID, so same-named agents differ and each keeps its own. */
export const agentHue = (agentDid: string) => (seedHash(agentDid) % 24) * 15;

export const fileAccess = (mode: string) =>
  ({ "read / write": "read and edit", "read-only": "read", off: "not touch" })[mode] ??
  mode;
export const bashAccess = (mode: string) =>
  ({ unrestricted: "run any", "read-only": "run read-only", off: "not run" })[mode] ??
  mode;
export const access = (mode: string) =>
  ({
    readwrite: "read and edit files",
    readonly: "read files",
    "meta-only": "use meta tools only",
    "read / write": "read and edit files",
    "read-only": "read files",
    off: "not touch files",
  })[mode] ?? mode;
const NETWORK: Record<string, string> = {
  enabled: "the network",
  inherit: "the network (inherited)",
};
export const network = (mode: string | null | undefined) =>
  (mode && NETWORK[mode]) || "no network";

const SHORT: Record<string, string> = {
  "read / write": "rw",
  "read-only": "ro",
  unrestricted: "any",
  off: "off",
};
export const shortAccess = (mode: string | null | undefined) =>
  SHORT[mode ?? "off"] ?? mode;
