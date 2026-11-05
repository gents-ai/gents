import type { Route } from "./router";

/** A place a `gents:` link names, from outside the app (`gents://session/<id>`)
    or inside a message (`gents:session/<id>`), optionally with the node that
    holds it (`?node=<did>`). Only sessions for now: a link to a message
    waits until a message's sequence is known to be the same on every
    replica of its session, since a link outlives the copy it was made on. */
export type AppLink = { kind: "session"; sessionId: string; nodeDid?: string };

const LINK = /^gents:(?:\/\/)?([a-z][a-z0-9-]*)\/([^?#]*)(?:\?([^#]*))?/i;

export function parseLink(href: string): AppLink | null {
  const match = LINK.exec(href.trim());
  if (!match) return null;
  const [, kind, path, query] = match;
  if (kind!.toLowerCase() !== "session") return null;
  const parts = path!.split("/").filter(Boolean);
  if (parts.length !== 1) return null;
  let sessionId: string;
  try {
    sessionId = decodeURIComponent(parts[0]!).trim();
  } catch {
    return null;
  }
  if (!sessionId) return null;
  const nodeDid = new URLSearchParams(query ?? "").get("node")?.trim();
  return { kind: "session", sessionId, ...(nodeDid ? { nodeDid } : {}) };
}

export const isAppLink = (href: string) => /^gents:/i.test(href.trim());

/** Where a link goes. Without a node, the session's owner is found among
    the nodes this app knows, as for any session route. */
export function linkRoute(link: AppLink): Route {
  return {
    name: "session",
    sessionId: link.sessionId,
    ...(link.nodeDid ? { nodeDid: link.nodeDid } : {}),
  };
}
