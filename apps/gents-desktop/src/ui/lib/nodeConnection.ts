import type { NodeView } from "../../hooks/fleetStore";

/** The node's live runtime connection (`DeploymentView.runtimeConnection`).
    A node the observer hasn't reconciled yet has no status and is still
    connecting. A transport failure keeps the node's last data; a refusal
    clears it. */
export type NodeConnection =
  "connecting" | "connected" | "disconnected" | "refused" | "noEndpoint";

type Observed = Pick<NodeView, "runtimeConnection">;

export function nodeConnection(node: Observed): NodeConnection {
  const connection = node.runtimeConnection;
  return connection === "connected" ||
    connection === "disconnected" ||
    connection === "refused" ||
    connection === "noEndpoint"
    ? connection
    : "connecting";
}

export const isOnline = (node: Observed) => nodeConnection(node) === "connected";

/** A word or two beside a node's name. */
export const CONNECTION_LABEL: Record<NodeConnection, string> = {
  connected: "Online",
  connecting: "Connecting",
  disconnected: "Offline",
  refused: "Refused",
  noEndpoint: "No address",
};

/** The sentence the composer, the pages and settings all use for a node
    that isn't connected. */
export function connectionTitle(
  connection: Exclude<NodeConnection, "connected">,
  name: string,
): string {
  switch (connection) {
    case "connecting":
      return `Connecting to ${name}`;
    case "disconnected":
      return `${name} is offline`;
    case "refused":
      return `${name} doesn't accept this app`;
    case "noEndpoint":
      return `${name} has no address this app can reach`;
  }
}

/** What a connection check says: the node's connection once it answered
    (`answer`, as `DeploymentView.runtimeConnection`), or that it hasn't
    answered yet. */
export function checkedConnectionText(answer: string | null, name: string): string {
  if (answer === null) return `${name} hasn't answered yet`;
  const connection = nodeConnection({ runtimeConnection: answer });
  return connection === "connected"
    ? `${name} is online`
    : connectionTitle(connection, name);
}

const UNAVAILABLE_DETAIL: Record<"disconnected" | "refused" | "noEndpoint", string> = {
  disconnected: "This shows once it's back.",
  refused:
    "Ask its owner to restore this device's access; it reconnects by itself once they do.",
  noEndpoint: "This shows once it has a runtime endpoint.",
};

export type PageConnection =
  | { kind: "loading"; title: string }
  | { kind: "unavailable"; title: string; detail: string }
  | { kind: "stale"; text: string };

/** What a page of one node's data says about the connection, or null when
    the page shows as it is. With nothing to show, the page loads while the
    node connects, or says why it can't, rather than saying the node has
    none. Data a lost connection kept is marked as possibly out of date. */
export function pageConnection(
  node: NodeView | null,
  hasData: boolean,
): PageConnection | null {
  if (!node) return null;
  const connection = nodeConnection(node);
  if (connection === "connected") return null;
  const title = connectionTitle(connection, node.node.displayName ?? node.label);
  if (connection === "connecting")
    return hasData ? null : { kind: "loading", title: `${title}…` };
  if (hasData) return { kind: "stale", text: `${title}. This may be out of date.` };
  return { kind: "unavailable", title, detail: UNAVAILABLE_DETAIL[connection] };
}
