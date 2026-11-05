import type { Route } from "./router";
import { isPackDigest, isPackName, isPackVersion, packPreviewKey } from "./packReview";

/** A place a `gents:` link names, from outside the app (`gents://session/<id>`)
    or inside a message (`gents:session/<id>`), optionally with the node that
    holds it (`?node=<did>`); or a pack to review installing
    (`gents://pack/<namespace>/<name>?version=<v>&digest=<sha256:…>`), whose
    digest pins the archive; or an invite to read before joining
    (`gents://invite/<token>`), kept whole since only the inviting node's
    runtime reads it, and opened in the Sharing section of the node this
    desktop runs, since that is the node that joins. A link to a message waits
    until a message's sequence is known to be the same on every replica of its
    session, since a link outlives the copy it was made on. */
export type AppLink =
  | { kind: "session"; sessionId: string; nodeDid?: string }
  | { kind: "pack"; pack: string; version?: string; digest?: string }
  | { kind: "invite"; invite: string };

const LINK = /^gents:(?:\/\/)?([a-z][a-z0-9-]*)\/([^?#]*)(?:\?([^#]*))?/i;

function decoded(part: string): string | null {
  try {
    return decodeURIComponent(part).trim();
  } catch {
    return null;
  }
}

function sessionLink(parts: string[], query: string | undefined): AppLink | null {
  if (parts.length !== 1) return null;
  const sessionId = decoded(parts[0]!);
  if (!sessionId) return null;
  const nodeDid = new URLSearchParams(query ?? "").get("node")?.trim();
  return { kind: "session", sessionId, ...(nodeDid ? { nodeDid } : {}) };
}

function packLink(parts: string[], query: string | undefined): AppLink | null {
  if (parts.length !== 2 || !parts.every(isPackName)) return null;
  const params = new URLSearchParams(query ?? "");
  const version = params.get("version")?.trim();
  const digest = params.get("digest")?.trim();
  if (version !== undefined && !isPackVersion(version)) return null;
  if (digest !== undefined && !isPackDigest(digest)) return null;
  return {
    kind: "pack",
    pack: parts.join("/"),
    ...(version ? { version } : {}),
    ...(digest ? { digest } : {}),
  };
}

/** The invite link's token format is assumed until the runtime defines how
    an invite is sent (#3472); replace this with that format. */
const INVITE_TOKEN = /^[A-Za-z0-9._~-]{1,512}$/;

function inviteLink(parts: string[], query: string | undefined): AppLink | null {
  if (parts.length !== 1 || !INVITE_TOKEN.test(parts[0]!)) return null;
  return {
    kind: "invite",
    invite: `gents://invite/${parts[0]}${query ? `?${query}` : ""}`,
  };
}

export function parseLink(href: string): AppLink | null {
  const match = LINK.exec(href.trim());
  if (!match) return null;
  const [, kind, path, query] = match;
  const parts = path!.split("/").filter(Boolean);
  switch (kind!.toLowerCase()) {
    case "session":
      return sessionLink(parts, query);
    case "pack":
      return packLink(parts, query);
    case "invite":
      return inviteLink(parts, query);
    default:
      return null;
  }
}

export const isAppLink = (href: string) => /^gents:/i.test(href.trim());

/** Where a link goes. Without a node, a session's owner is found among the
    nodes this app knows, as for any session route. */
export function linkRoute(link: Exclude<AppLink, { kind: "invite" }>): Route {
  if (link.kind === "pack")
    return {
      name: "packs",
      review: packPreviewKey({
        package: link.version ? `${link.pack}@${link.version}` : link.pack,
        source: "link",
        ...(link.digest ? { digest: link.digest } : {}),
      }),
    };
  return {
    name: "session",
    sessionId: link.sessionId,
    ...(link.nodeDid ? { nodeDid: link.nodeDid } : {}),
  };
}
